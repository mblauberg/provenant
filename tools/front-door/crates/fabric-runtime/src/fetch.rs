//! Bounded, manifest-verified publication of run artefacts.
//!
//! Paths are relative UTF-8 file names. Contents are verified before any
//! filesystem mutation, then written into a private same-filesystem staging
//! directory and published as one directory rename.
use provenant_core::{Digest, HostId, RelativePath, RunId};
use provenant_os::fs::{self, Directory, FileKind, FsError, PublishMode};
use provenant_records::{CanonicalV1, FileDigest};
use serde::{Deserialize, Serialize};
use sha2::{Digest as Sha2Digest, Sha256};
use std::{collections::BTreeMap, ffi::OsStr};
use thiserror::Error;

pub const MAX_FETCH_FILES: usize = 16;
pub const MAX_FETCH_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_FETCH_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_FETCH_CHUNKS: usize = 4096;
pub const MAX_FETCH_MANIFEST_BYTES: usize = 1024 * 1024;
const FETCH_VERSION: u8 = 1;
const DIRECTORY_MODE: u32 = 0o700;
const MAX_CLEANUP_DEPTH: usize = 64;
const MAX_CLEANUP_DIRECTORY_ENTRIES: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchFile {
    pub path: String,
    pub size: u64,
    pub digest: FileDigest,
    #[serde(flatten)]
    pub extras: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchManifest {
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
    pub version: u8,
    pub files: Vec<FetchFile>,
    pub digest: Digest,
}

#[derive(Serialize)]
struct ManifestBody<'a> {
    #[serde(flatten)]
    extra: &'a BTreeMap<String, serde_json::Value>,
    version: u8,
    files: &'a [FetchFile],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchChunk {
    pub path: String,
    pub offset: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedFetch {
    pub host: HostId,
    pub run: RunId,
    pub digest: Digest,
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("fetch manifest or transfer violates its bound")]
    Invalid(&'static str),
    #[error("fetch manifest JSON is invalid")]
    Json(#[from] serde_json::Error),
    #[error("fetch record cannot be canonically encoded")]
    Canonical(#[from] provenant_records::CanonicalError),
    #[error("safe filesystem operation failed")]
    Filesystem(#[from] FsError),
    #[error("fetch directory is not a private owner directory")]
    UnsafeDirectory,
    #[error("fetch file digest did not match: {path}")]
    FileDigest { path: String },
    #[error("fetch destination already exists for host {host} and run {run}")]
    DestinationExists { host: HostId, run: RunId },
    #[error("fetch directory is visible but directory sync failed for host {host} and run {run}")]
    VisibleDurabilityUnconfirmed {
        host: HostId,
        run: RunId,
        #[source]
        source: FsError,
    },
}

type Result<T> = std::result::Result<T, FetchError>;

impl FetchManifest {
    /// Build a canonical version 1 manifest. Entries are ordered by path.
    pub fn new(mut files: Vec<FetchFile>) -> Result<Self> {
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let extra = BTreeMap::new();
        let digest = manifest_digest(FETCH_VERSION, &files, &extra)?;
        let manifest = Self {
            extra,
            version: FETCH_VERSION,
            files,
            digest,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_FETCH_MANIFEST_BYTES {
            return Err(FetchError::Invalid("manifest bytes"));
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_FETCH_MANIFEST_BYTES {
            return Err(FetchError::Invalid("manifest bytes"));
        }
        let manifest: Self = serde_json::from_slice(bytes)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != FETCH_VERSION || self.files.len() > MAX_FETCH_FILES {
            return Err(FetchError::Invalid("manifest version or file count"));
        }
        let mut total = 0_u64;
        let mut prior: Option<&str> = None;
        for file in &self.files {
            validate_relative_file(&file.path)?;
            if prior.is_some_and(|path| path >= file.path.as_str()) {
                return Err(FetchError::Invalid(
                    "manifest paths are not strictly ordered",
                ));
            }
            if prior.is_some_and(|path| file.path.starts_with(&format!("{path}/"))) {
                return Err(FetchError::Invalid("file path is also a directory"));
            }
            total = total
                .checked_add(file.size)
                .filter(|bytes| *bytes <= MAX_FETCH_ARTIFACT_BYTES as u64)
                .ok_or(FetchError::Invalid("total file bytes"))?;
            prior = Some(&file.path);
        }
        // A later path can be the prefix of an earlier one only if ordering is
        // malformed, which the strict-order check above already rejects.
        if manifest_digest(self.version, &self.files, &self.extra)? != self.digest {
            return Err(FetchError::Invalid("manifest digest"));
        }
        Ok(())
    }
}

fn manifest_digest(
    version: u8,
    files: &[FetchFile],
    extra: &BTreeMap<String, serde_json::Value>,
) -> Result<Digest> {
    Ok(CanonicalV1::record_digest(
        "fabric-fetch-manifest",
        1,
        &ManifestBody {
            version,
            files,
            extra,
        },
    )?)
}

fn validate_relative_file(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 4096
        || path.split('/').count() > MAX_CLEANUP_DEPTH
        || path.contains('\\')
        || path.contains(':')
        || path.bytes().any(|byte| byte.is_ascii_control())
        || path.split('/').any(str::is_empty)
    {
        return Err(FetchError::Invalid("relative file path"));
    }
    let relative =
        RelativePath::parse(path).map_err(|_| FetchError::Invalid("relative file path"))?;
    if relative.as_path().to_str() != Some(path) {
        return Err(FetchError::Invalid("relative file path"));
    }
    for component in path.split('/') {
        if component.eq_ignore_ascii_case(".git")
            || fs::is_reserved_lock_name(OsStr::new(component))
            || component == "."
            || component == ".."
        {
            return Err(FetchError::Invalid("reserved file path component"));
        }
    }
    Ok(())
}

/// Verify the complete bounded transfer, then publish it beneath `fetch_root`
/// as `fetch_root/<host>/<run>`. `fetch_root` must be a private directory
/// opened through OS crate's safe filesystem API.
pub fn publish_fetch(
    fetch_root: &Directory,
    host: HostId,
    run: RunId,
    manifest: &FetchManifest,
    chunks: Vec<FetchChunk>,
) -> Result<PublishedFetch> {
    manifest.validate()?;
    let contents = verify_chunks(manifest, chunks)?;
    require_private(fetch_root)?;

    let host_name = host.to_string();
    let run_name = run.to_string();
    let host_dir = open_or_create_directory(fetch_root, &host_name)?;
    let (stage_name, stage) = create_stage(&host_dir)?;
    let write_result = (|| {
        for (file, bytes) in manifest.files.iter().zip(contents.iter()) {
            let components: Vec<&str> = file.path.split('/').collect();
            write_file(&stage, &components, bytes)?;
        }
        stage.sync()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = remove_tree(&host_dir, OsStr::new(&stage_name));
        return Err(error);
    }

    match host_dir.rename_noreplace(OsStr::new(&stage_name), &host_dir, OsStr::new(&run_name)) {
        Ok(()) => {}
        Err(error) if error.is_exists() => {
            let _ = remove_tree(&host_dir, OsStr::new(&stage_name));
            return Err(FetchError::DestinationExists { host, run });
        }
        Err(error) => {
            let _ = remove_tree(&host_dir, OsStr::new(&stage_name));
            return Err(error.into());
        }
    }
    match host_dir.sync() {
        Ok(()) => Ok(PublishedFetch {
            host,
            run,
            digest: manifest.digest,
        }),
        Err(source) => Err(FetchError::VisibleDurabilityUnconfirmed { host, run, source }),
    }
}

fn verify_chunks(manifest: &FetchManifest, chunks: Vec<FetchChunk>) -> Result<Vec<Vec<u8>>> {
    if chunks.len() > MAX_FETCH_CHUNKS {
        return Err(FetchError::Invalid("chunk count"));
    }
    let mut indexes = BTreeMap::new();
    let mut contents = Vec::with_capacity(manifest.files.len());
    let mut offsets = Vec::with_capacity(manifest.files.len());
    for (index, file) in manifest.files.iter().enumerate() {
        indexes.insert(file.path.as_str(), index);
        contents.push(Vec::with_capacity(file.size as usize));
        offsets.push(0_u64);
    }
    for chunk in chunks {
        let index = *indexes
            .get(chunk.path.as_str())
            .ok_or(FetchError::Invalid("unlisted fetch file"))?;
        if chunk.bytes.is_empty() || chunk.bytes.len() > MAX_FETCH_CHUNK_BYTES {
            return Err(FetchError::Invalid("chunk size"));
        }
        if chunk.offset != offsets[index] {
            return Err(FetchError::Invalid("non-contiguous file chunks"));
        }
        let next = offsets[index]
            .checked_add(chunk.bytes.len() as u64)
            .filter(|size| *size <= manifest.files[index].size)
            .ok_or(FetchError::Invalid("file size"))?;
        contents[index].extend_from_slice(&chunk.bytes);
        offsets[index] = next;
    }
    for (index, file) in manifest.files.iter().enumerate() {
        if offsets[index] != file.size || contents[index].len() as u64 != file.size {
            return Err(FetchError::Invalid("incomplete fetch file"));
        }
        let digest = FileDigest::from_bytes(Sha256::digest(&contents[index]).into());
        if digest != file.digest {
            return Err(FetchError::FileDigest {
                path: file.path.clone(),
            });
        }
    }
    Ok(contents)
}

fn require_private(directory: &Directory) -> Result<()> {
    let metadata = directory.metadata()?;
    if metadata.kind != FileKind::Directory
        || metadata.uid != fs::effective_uid()
        || metadata.mode & 0o077 != 0
    {
        return Err(FetchError::UnsafeDirectory);
    }
    Ok(())
}

fn open_or_create_directory(parent: &Directory, name: &str) -> Result<Directory> {
    match parent.mkdir(OsStr::new(name), DIRECTORY_MODE) {
        Ok(()) => parent.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    let child = parent.open_verified_directory(OsStr::new(name))?;
    require_private(&child)?;
    Ok(child)
}

fn create_stage(parent: &Directory) -> Result<(String, Directory)> {
    for _ in 0..32 {
        let name = format!(
            ".provenant-fetch-stage-{}",
            provenant_core::hex(&fs::entropy_128()?)
        );
        match parent.mkdir(OsStr::new(&name), DIRECTORY_MODE) {
            Ok(()) => {
                parent.sync()?;
                let directory = parent.open_verified_directory(OsStr::new(&name))?;
                require_private(&directory)?;
                return Ok((name, directory));
            }
            Err(error) if error.is_exists() => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(FetchError::Invalid("staging name exhaustion"))
}

fn write_file(parent: &Directory, components: &[&str], bytes: &[u8]) -> Result<()> {
    let (name, tail) = components
        .split_first()
        .ok_or(FetchError::Invalid("empty relative file path"))?;
    if tail.is_empty() {
        parent
            .atomic_write(
                OsStr::new(name),
                ".provenant-fetch-file-",
                bytes,
                PublishMode::NoReplace,
            )?
            .require_durable()
            .map_err(|_| FetchError::Invalid("staged file durability"))
    } else {
        let child = open_or_create_directory(parent, name)?;
        write_file(&child, tail, bytes)
    }
}

/// Remove only a private staging tree. Symlinks are unlinked without following
/// them; unexpected entry types stop cleanup rather than widening traversal.
fn remove_tree(parent: &Directory, name: &OsStr) -> Result<()> {
    remove_tree_at(parent, name, 0)
}

fn remove_tree_at(parent: &Directory, name: &OsStr, depth: usize) -> Result<()> {
    if depth >= MAX_CLEANUP_DEPTH {
        return Err(FetchError::Invalid("cleanup depth"));
    }
    let directory = parent.open_verified_directory(name)?;
    require_private(&directory)?;
    for entry in fs::entry_names(&directory, MAX_CLEANUP_DIRECTORY_ENTRIES)? {
        let metadata = directory.metadata_at(&entry, false)?;
        match metadata.kind {
            FileKind::Directory => remove_tree_at(&directory, &entry, depth + 1)?,
            FileKind::Regular if metadata.links == 1 => directory.unlink(&entry)?,
            FileKind::Symlink => directory.unlink(&entry)?,
            _ => return Err(FetchError::UnsafeDirectory),
        }
    }
    directory.sync()?;
    parent.rmdir(name)?;
    parent.sync()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs as host_fs, os::unix::fs::PermissionsExt, path::Path};

    struct Fixture {
        root: tempfile::TempDir,
        directory: Directory,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            host_fs::set_permissions(root.path(), host_fs::Permissions::from_mode(0o700)).unwrap();
            let directory = Directory::open_anchor(root.path()).unwrap();
            Self { root, directory }
        }
    }

    fn file(path: &str, bytes: &[u8]) -> FetchFile {
        FetchFile {
            path: path.to_owned(),
            size: bytes.len() as u64,
            digest: FileDigest::from_bytes(Sha256::digest(bytes).into()),
            extras: BTreeMap::new(),
        }
    }

    fn chunk(path: &str, offset: u64, bytes: &[u8]) -> FetchChunk {
        FetchChunk {
            path: path.to_owned(),
            offset,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn manifest_preserves_digest_with_future_file_fields() {
        let mut entry = file("output", b"payload");
        entry
            .extras
            .insert("future".into(), serde_json::json!({"nested": true}));
        let manifest = FetchManifest::new(vec![entry]).unwrap();
        let decoded = FetchManifest::decode(&manifest.encode().unwrap()).unwrap();
        assert_eq!(decoded.digest, manifest.digest);
        assert_eq!(decoded.files, manifest.files);
    }

    #[test]
    fn manifest_preserves_digest_with_future_manifest_fields() {
        let manifest = FetchManifest::new(vec![file("output", b"payload")]).unwrap();
        let mut newer = serde_json::to_value(manifest).unwrap();
        newer["future"] = serde_json::json!({"nested":[null,u64::MAX]});
        let mut body = newer.clone();
        body.as_object_mut().unwrap().remove("digest");
        let digest = CanonicalV1::record_digest("fabric-fetch-manifest", 1, &body).unwrap();
        newer["digest"] = serde_json::json!(digest.to_string());
        let decoded = FetchManifest::decode(&serde_json::to_vec(&newer).unwrap()).unwrap();
        assert_eq!(decoded.digest, digest);
        assert_eq!(
            CanonicalV1::bytes(&decoded).unwrap(),
            CanonicalV1::bytes(&newer).unwrap()
        );
        let encoded = decoded.encode().unwrap();
        assert_eq!(FetchManifest::decode(&encoded).unwrap(), decoded);
        let mut tampered = newer;
        tampered["future"]["nested"] = serde_json::json!([1]);
        assert!(matches!(
            FetchManifest::decode(&serde_json::to_vec(&tampered).unwrap()),
            Err(FetchError::Invalid("manifest digest"))
        ));
    }

    #[test]
    fn manifest_depth_fits_cleanup_limit() {
        let deepest = format!("{}file", "d/".repeat(MAX_CLEANUP_DEPTH - 1));
        assert!(FetchManifest::new(vec![file(&deepest, b"payload")]).is_ok());
        let too_deep = format!("d/{deepest}");
        assert!(matches!(
            FetchManifest::new(vec![file(&too_deep, b"payload")]),
            Err(FetchError::Invalid(_))
        ));
    }

    #[test]
    fn cleanup_removes_large_tree_without_following_symlinks() {
        let fixture = Fixture::new();
        let stage = fixture.root.path().join("stage");
        host_fs::create_dir(&stage).unwrap();
        host_fs::set_permissions(&stage, host_fs::Permissions::from_mode(0o700)).unwrap();
        for index in 0..2048 {
            host_fs::write(stage.join(format!("artifact-{index:04}")), b"payload").unwrap();
        }
        let outside = fixture.root.path().join("keep");
        host_fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, stage.join("link")).unwrap();

        remove_tree(&fixture.directory, OsStr::new("stage")).unwrap();
        assert!(!stage.exists());
        assert_eq!(host_fs::read(outside).unwrap(), b"outside");
    }

    #[test]
    fn cleanup_refuses_hard_links() {
        let fixture = Fixture::new();
        let stage = fixture.root.path().join("stage");
        host_fs::create_dir(&stage).unwrap();
        host_fs::set_permissions(&stage, host_fs::Permissions::from_mode(0o700)).unwrap();
        let outside = fixture.root.path().join("keep");
        host_fs::write(&outside, b"outside").unwrap();
        host_fs::hard_link(&outside, stage.join("hard-link")).unwrap();
        assert!(matches!(
            remove_tree(&fixture.directory, OsStr::new("stage")),
            Err(FetchError::UnsafeDirectory)
        ));
        assert_eq!(host_fs::read(&outside).unwrap(), b"outside");
        assert!(stage.join("hard-link").is_file());
    }

    #[test]
    fn cleanup_refuses_excessive_depth() {
        let fixture = Fixture::new();
        let mut nested = fixture.root.path().join("stage");
        host_fs::create_dir(&nested).unwrap();
        host_fs::set_permissions(&nested, host_fs::Permissions::from_mode(0o700)).unwrap();
        for _ in 0..MAX_CLEANUP_DEPTH {
            nested.push("nested");
            host_fs::create_dir(&nested).unwrap();
            host_fs::set_permissions(&nested, host_fs::Permissions::from_mode(0o700)).unwrap();
        }
        host_fs::write(nested.join("keep"), b"too deep").unwrap();
        assert!(matches!(
            remove_tree(&fixture.directory, OsStr::new("stage")),
            Err(FetchError::Invalid("cleanup depth"))
        ));
        assert_eq!(host_fs::read(nested.join("keep")).unwrap(), b"too deep");
    }

    #[test]
    fn verifies_chunks_then_publishes_complete_tree_atomically() {
        let fixture = Fixture::new();
        let host = HostId::from_bytes([1; 16]);
        let run = RunId::from_bytes([2; 16]);
        let manifest = FetchManifest::new(vec![
            file("logs/out.txt", b"hello"),
            file("result.json", b"{}"),
        ])
        .unwrap();
        let encoded = manifest.encode().unwrap();
        assert_eq!(FetchManifest::decode(&encoded).unwrap(), manifest);
        let published = publish_fetch(
            &fixture.directory,
            host,
            run,
            &manifest,
            vec![
                chunk("result.json", 0, b"{}"),
                chunk("logs/out.txt", 0, b"hello"),
            ],
        )
        .unwrap();
        assert_eq!(published.digest, manifest.digest);
        let host_dir = fixture
            .directory
            .open_verified_directory(OsStr::new(&host.to_string()))
            .unwrap();
        let run_dir = host_dir
            .open_verified_directory(OsStr::new(&run.to_string()))
            .unwrap();
        let logs = run_dir.open_verified_directory(OsStr::new("logs")).unwrap();
        assert_eq!(
            logs.read_bounded(OsStr::new("out.txt"), 16).unwrap(),
            b"hello"
        );
        assert_eq!(
            run_dir.read_bounded(OsStr::new("result.json"), 16).unwrap(),
            b"{}"
        );
        assert!(matches!(
            publish_fetch(&fixture.directory, host, run, &manifest, vec![]),
            Err(FetchError::Invalid(_))
        ));
    }

    #[test]
    fn refuses_traversal_digest_mismatch_and_non_contiguous_chunks_before_publish() {
        let fixture = Fixture::new();
        for path in [
            "../escape",
            "a/../escape",
            ".git/config",
            "a/b.lock",
            "a/b.LOCK",
            "B.Lock/c",
            "lock",
            "a/b.loc\u{212A}",
            "/absolute",
            "a//b",
        ] {
            let files = vec![file(path, b"x")];
            let digest = manifest_digest(FETCH_VERSION, &files, &BTreeMap::new()).unwrap();
            let manifest = FetchManifest {
                extra: Default::default(),
                version: FETCH_VERSION,
                files,
                digest,
            };
            assert!(manifest.validate().is_err(), "{path}");
        }
        let host = HostId::from_bytes([3; 16]);
        let run = RunId::from_bytes([4; 16]);
        let manifest = FetchManifest::new(vec![file("out", b"hello")]).unwrap();
        assert!(matches!(
            publish_fetch(
                &fixture.directory,
                host,
                run,
                &manifest,
                vec![chunk("out", 0, b"jello")]
            ),
            Err(FetchError::FileDigest { .. })
        ));
        assert!(matches!(
            publish_fetch(
                &fixture.directory,
                host,
                run,
                &manifest,
                vec![chunk("out", 1, b"hello")]
            ),
            Err(FetchError::Invalid("non-contiguous file chunks"))
        ));
        assert!(
            !Path::new(
                &fixture
                    .root
                    .path()
                    .join(host.to_string())
                    .join(run.to_string())
            )
            .exists()
        );
    }

    #[test]
    fn refuses_a_symlink_host_directory() {
        let fixture = Fixture::new();
        let host = HostId::from_bytes([5; 16]);
        let target = fixture.root.path().join("target");
        host_fs::create_dir(&target).unwrap();
        host_fs::set_permissions(&target, host_fs::Permissions::from_mode(0o700)).unwrap();
        fixture
            .directory
            .symlink(&target, OsStr::new(&host.to_string()))
            .unwrap();
        let manifest = FetchManifest::new(vec![file("out", b"ok")]).unwrap();
        assert!(
            publish_fetch(
                &fixture.directory,
                host,
                RunId::from_bytes([6; 16]),
                &manifest,
                vec![chunk("out", 0, b"ok")],
            )
            .is_err()
        );
        assert!(!target.join("00000000000000000000000000000006").exists());
    }
}
