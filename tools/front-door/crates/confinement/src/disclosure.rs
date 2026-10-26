//! Bounded, descriptor-relative disclosure scan.
//!
//! Prompt-training callers use this before exposing a prompt to a run. Path
//! overlap is refused before any traversal; every candidate is then scanned
//! through held `Directory`/`FileHandle` capabilities with strict file and byte
//! budgets. Secret-pattern matches and incomplete observations refuse.
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::Path;

use provenant_os::exec::CaptureFilter;
use provenant_os::fs::{Directory, FileKind, Metadata};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::permissions::{PathGrant, PathScope, PermissionError, validate_noncredential_grant};
use crate::redact::{Redactor, RedactorError};

pub const MAX_DISCLOSURE_FILES: u64 = 10_000;
pub const MAX_DISCLOSURE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: u64 = 20_000;
const MAX_DISCLOSURE_ROOTS: usize = 128;
const MAX_NAME_BYTES: usize = 255;
const MAX_DEPTH: usize = 64;
const BUFFER_SIZE: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum DisclosureError {
    #[error("disclosure path is invalid")]
    InvalidPath,
    #[error("prompt, cwd or read root overlaps protected data")]
    ProtectedOverlap,
    #[error("disclosure scan exceeded its file or byte budget")]
    BudgetExceeded,
    #[error("disclosure scan encountered a link or special file")]
    UnsafeEntry,
    #[error("disclosure scan observed an entry change")]
    ChangedEntry,
    #[error("disclosure scan found content matching a secret pattern")]
    SecretMatch,
    #[error("disclosure secret set is invalid")]
    InvalidSecretSet(#[source] RedactorError),
    #[error("disclosure filesystem operation failed")]
    Filesystem(#[source] provenant_os::fs::FsError),
    #[error("disclosure scan read failed")]
    Read(#[source] std::io::Error),
    #[error("disclosure scan digest encoding failed")]
    DigestBound,
    #[error("disclosure root grant is invalid")]
    Grant(#[source] PermissionError),
}

/// Filesystem scope whose contents may be disclosed to a prompt-training
/// route. The prompt is scanned as an exact file unless a selected subtree
/// already covers it.
pub struct DisclosureRequest<'a> {
    pub prompt_path: &'a Path,
    pub cwd: &'a PathGrant,
    pub read_roots: &'a [PathGrant],
    pub protected_roots: &'a [PathGrant],
    pub known_secrets: &'a [Vec<u8>],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosureReceipt {
    pub files_scanned: u64,
    pub bytes_scanned: u64,
    pub content_digest: String,
}

/// Scan candidate roots in deterministic path order. The digest commits to
/// path bytes and file contents but contains no source text.
pub fn scan_disclosure(
    request: &DisclosureRequest<'_>,
) -> Result<DisclosureReceipt, DisclosureError> {
    let prompt = PathGrant {
        path: request.prompt_path.to_owned(),
        scope: PathScope::Exact,
    };
    validate_noncredential_grant(&prompt).map_err(DisclosureError::Grant)?;
    validate_noncredential_grant(request.cwd).map_err(DisclosureError::Grant)?;
    if request.cwd.scope != PathScope::Subtree {
        return Err(DisclosureError::InvalidPath);
    }
    if request.read_roots.len() > MAX_DISCLOSURE_ROOTS
        || request.protected_roots.len() > MAX_DISCLOSURE_ROOTS
    {
        return Err(DisclosureError::BudgetExceeded);
    }
    for root in request.read_roots.iter().chain(request.protected_roots) {
        validate_noncredential_grant(root).map_err(DisclosureError::Grant)?;
    }
    require_canonical_path(request.prompt_path)?;
    for grant in std::iter::once(request.cwd)
        .chain(request.read_roots)
        .chain(request.protected_roots)
    {
        require_canonical_path(&grant.path)?;
    }

    let mut candidates = Vec::with_capacity(request.read_roots.len() + 2);
    candidates.push(request.cwd.clone());
    candidates.push(prompt.clone());
    candidates.extend(request.read_roots.iter().cloned());
    for candidate in &candidates {
        if request
            .protected_roots
            .iter()
            .any(|protected| candidate.overlaps(protected))
        {
            return Err(DisclosureError::ProtectedOverlap);
        }
    }

    for protected in request.protected_roots {
        verify_grant(protected)?;
    }

    candidates.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.scope.cmp(&right.scope))
    });
    candidates.dedup_by(|left, right| left.path == right.path && left.scope == right.scope);
    if candidates
        .windows(2)
        .any(|pair| pair[0].path == pair[1].path && pair[0].scope != pair[1].scope)
    {
        return Err(DisclosureError::InvalidPath);
    }
    let selected: Vec<PathGrant> = candidates
        .iter()
        .filter(|candidate| {
            !candidates.iter().any(|other| {
                other.scope == PathScope::Subtree
                    && other.path != candidate.path
                    && candidate.path.starts_with(&other.path)
            })
        })
        .cloned()
        .collect();

    let mut scanner = Scanner {
        redactor: Redactor::new(request.known_secrets.iter().cloned())
            .map_err(DisclosureError::InvalidSecretSet)?,
        files: 0,
        bytes: 0,
        entries: 0,
        digest: Sha256::new(),
    };
    for candidate in selected {
        scanner.scan_root(&candidate)?;
    }
    let digest = scanner.digest.finalize();
    let mut content_digest = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut content_digest, "{byte:02x}").map_err(|_| DisclosureError::DigestBound)?;
    }
    Ok(DisclosureReceipt {
        files_scanned: scanner.files,
        bytes_scanned: scanner.bytes,
        content_digest,
    })
}

/// Scan one exact in-memory snapshot, such as a prompt about to be sealed,
/// with the same secret patterns. A match refuses without naming the text.
pub fn scan_snapshot(bytes: &[u8], known_secrets: &[Vec<u8>]) -> Result<(), DisclosureError> {
    let mut redactor =
        Redactor::new(known_secrets.iter().cloned()).map_err(DisclosureError::InvalidSecretSet)?;
    for chunk in bytes.chunks(BUFFER_SIZE) {
        redactor
            .filter(chunk)
            .map_err(|_| DisclosureError::SecretMatch)?;
        if redactor.has_redaction() {
            return Err(DisclosureError::SecretMatch);
        }
    }
    redactor
        .finish()
        .map_err(|_| DisclosureError::SecretMatch)?;
    if redactor.has_redaction() {
        return Err(DisclosureError::SecretMatch);
    }
    Ok(())
}

struct Scanner {
    redactor: Redactor,
    files: u64,
    bytes: u64,
    entries: u64,
    digest: Sha256,
}

enum OpenedGrant {
    Directory(Directory),
    ExactFile { parent: Directory, name: OsString },
}

fn open_grant(grant: &PathGrant) -> Result<OpenedGrant, DisclosureError> {
    match grant.scope {
        PathScope::Subtree => Directory::open_anchor(&grant.path)
            .map(OpenedGrant::Directory)
            .map_err(DisclosureError::Filesystem),
        PathScope::Exact => {
            let parent = grant.path.parent().ok_or(DisclosureError::InvalidPath)?;
            let name = grant
                .path
                .file_name()
                .ok_or(DisclosureError::InvalidPath)?
                .to_owned();
            let parent = Directory::open_anchor(parent).map_err(DisclosureError::Filesystem)?;
            Ok(OpenedGrant::ExactFile { parent, name })
        }
    }
}

impl Scanner {
    fn scan_root(&mut self, grant: &PathGrant) -> Result<(), DisclosureError> {
        match open_grant(grant)? {
            OpenedGrant::Directory(directory) => {
                if directory
                    .metadata()
                    .map_err(DisclosureError::Filesystem)?
                    .kind
                    != FileKind::Directory
                {
                    return Err(DisclosureError::UnsafeEntry);
                }
                let mut stack = vec![(grant.path.clone(), directory, 0usize)];
                while let Some((path, directory, depth)) = stack.pop() {
                    if depth > MAX_DEPTH {
                        return Err(DisclosureError::BudgetExceeded);
                    }
                    let mut names: Vec<OsString> = Vec::new();
                    for name in directory.entries().map_err(DisclosureError::Filesystem)? {
                        let name = name.map_err(DisclosureError::Filesystem)?;
                        if name.as_encoded_bytes().len() > MAX_NAME_BYTES {
                            return Err(DisclosureError::UnsafeEntry);
                        }
                        self.entries = self
                            .entries
                            .checked_add(1)
                            .ok_or(DisclosureError::BudgetExceeded)?;
                        if self.entries > MAX_DIRECTORY_ENTRIES {
                            return Err(DisclosureError::BudgetExceeded);
                        }
                        names.push(name);
                    }
                    names.sort();
                    let mut child_directories = Vec::new();
                    for name in names {
                        let metadata = directory
                            .metadata_at(&name, false)
                            .map_err(DisclosureError::Filesystem)?;
                        let child_path = path.join(&name);
                        match metadata.kind {
                            FileKind::Directory => {
                                if depth == MAX_DEPTH {
                                    return Err(DisclosureError::BudgetExceeded);
                                }
                                let child = directory
                                    .open_verified_directory(&name)
                                    .map_err(DisclosureError::Filesystem)?;
                                let opened =
                                    child.metadata().map_err(DisclosureError::Filesystem)?;
                                if !opened.same_identity(&metadata) {
                                    return Err(DisclosureError::ChangedEntry);
                                }
                                child_directories.push((child_path, child, depth + 1));
                            }
                            FileKind::Regular => {
                                self.scan_file(&directory, &name, &child_path, metadata)?;
                            }
                            FileKind::Symlink | FileKind::Other => {
                                return Err(DisclosureError::UnsafeEntry);
                            }
                        }
                    }
                    stack.extend(child_directories.into_iter().rev());
                }
            }
            OpenedGrant::ExactFile { parent, name } => {
                let metadata = parent
                    .metadata_at(&name, false)
                    .map_err(DisclosureError::Filesystem)?;
                self.scan_file(&parent, &name, &grant.path, metadata)?;
            }
        }
        Ok(())
    }

    fn scan_file(
        &mut self,
        directory: &Directory,
        name: &OsStr,
        absolute: &Path,
        before: Metadata,
    ) -> Result<(), DisclosureError> {
        if before.kind != FileKind::Regular || before.links != 1 {
            return Err(DisclosureError::UnsafeEntry);
        }
        let next_files = self
            .files
            .checked_add(1)
            .ok_or(DisclosureError::BudgetExceeded)?;
        if next_files > MAX_DISCLOSURE_FILES {
            return Err(DisclosureError::BudgetExceeded);
        }
        let remaining = MAX_DISCLOSURE_BYTES
            .checked_sub(self.bytes)
            .ok_or(DisclosureError::BudgetExceeded)?;
        if before.size > remaining {
            return Err(DisclosureError::BudgetExceeded);
        }
        let mut file = directory
            .open_read(name)
            .map_err(DisclosureError::Filesystem)?;
        let opened = file.metadata().map_err(DisclosureError::Filesystem)?;
        if !opened.same_identity(&before) || opened.kind != FileKind::Regular {
            return Err(DisclosureError::ChangedEntry);
        }
        let path_bytes = absolute.as_os_str().as_encoded_bytes();
        let path_len = u64::try_from(path_bytes.len()).map_err(|_| DisclosureError::DigestBound)?;
        self.digest.update(path_len.to_le_bytes());
        self.digest.update(path_bytes);

        let mut buffer = [0u8; BUFFER_SIZE];
        let mut file_bytes = 0u64;
        loop {
            let left = MAX_DISCLOSURE_BYTES
                .checked_sub(self.bytes)
                .ok_or(DisclosureError::BudgetExceeded)?;
            if left == 0 {
                let mut probe = [0u8; 1];
                if file.read(&mut probe).map_err(DisclosureError::Read)? != 0 {
                    return Err(DisclosureError::BudgetExceeded);
                }
                break;
            }
            let left = match usize::try_from(left) {
                Ok(value) => value,
                Err(_) => usize::MAX,
            };
            let amount = buffer.len().min(left);
            let count = file
                .read(&mut buffer[..amount])
                .map_err(DisclosureError::Read)?;
            if count == 0 {
                break;
            }
            let input = &buffer[..count];
            self.redactor
                .filter(input)
                .map_err(|_| DisclosureError::SecretMatch)?;
            if self.redactor.has_redaction() {
                return Err(DisclosureError::SecretMatch);
            }
            let count = u64::try_from(count).map_err(|_| DisclosureError::BudgetExceeded)?;
            self.bytes = self
                .bytes
                .checked_add(count)
                .ok_or(DisclosureError::BudgetExceeded)?;
            file_bytes = file_bytes
                .checked_add(count)
                .ok_or(DisclosureError::BudgetExceeded)?;
            self.digest.update(input);
        }
        self.redactor
            .finish()
            .map_err(|_| DisclosureError::SecretMatch)?;
        if self.redactor.has_redaction() {
            return Err(DisclosureError::SecretMatch);
        }
        let after = file.metadata().map_err(DisclosureError::Filesystem)?;
        if !after.same_identity(&opened) || after.size != opened.size || file_bytes != opened.size {
            return Err(DisclosureError::ChangedEntry);
        }
        self.files = next_files;
        Ok(())
    }
}

fn verify_grant(grant: &PathGrant) -> Result<(), DisclosureError> {
    match open_grant(grant)? {
        OpenedGrant::Directory(directory) => {
            let metadata = directory.metadata().map_err(DisclosureError::Filesystem)?;
            if metadata.kind != FileKind::Directory {
                return Err(DisclosureError::UnsafeEntry);
            }
        }
        OpenedGrant::ExactFile { parent, name } => {
            let metadata = parent
                .metadata_at(&name, false)
                .map_err(DisclosureError::Filesystem)?;
            if metadata.kind != FileKind::Regular || metadata.links != 1 {
                return Err(DisclosureError::UnsafeEntry);
            }
        }
    }
    Ok(())
}

fn require_canonical_path(path: &Path) -> Result<(), DisclosureError> {
    let canonical = provenant_os::fs::canonicalize(path).map_err(DisclosureError::Filesystem)?;
    if canonical.as_path() != path {
        return Err(DisclosureError::InvalidPath);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct DisclosureFixture {
        _temp: TempDir,
        work: PathBuf,
        prompt: PathBuf,
        cwd: PathGrant,
    }

    impl DisclosureFixture {
        fn new() -> Self {
            let temp = crate::test_support::tempdir().expect("temporary disclosure root");
            let work = temp.path().join("work");
            fs::create_dir(&work).expect("work directory");
            let prompt = temp.path().join("prompt.txt");
            fs::write(&prompt, b"prompt").expect("prompt file");
            let prompt = provenant_os::fs::canonicalize(&prompt).expect("canonical prompt");
            let cwd = PathGrant {
                path: provenant_os::fs::canonicalize(&work).expect("canonical work"),
                scope: PathScope::Subtree,
            };
            Self {
                _temp: temp,
                work,
                prompt,
                cwd,
            }
        }
    }

    fn request<'a>(
        prompt: &'a Path,
        cwd: &'a PathGrant,
        read_roots: &'a [PathGrant],
        protected_roots: &'a [PathGrant],
        known_secrets: &'a [Vec<u8>],
    ) -> DisclosureRequest<'a> {
        DisclosureRequest {
            prompt_path: prompt,
            cwd,
            read_roots,
            protected_roots,
            known_secrets,
        }
    }

    #[test]
    fn protected_overlap_refuses_after_canonical_path_binding() {
        let fixture = DisclosureFixture::new();
        let protected_path = fixture.work.join("AGENTS.md");
        fs::write(&protected_path, b"instructions").expect("instruction file");
        let prompt_path = &fixture.prompt;
        let cwd = &fixture.cwd;
        let protected = [PathGrant {
            path: provenant_os::fs::canonicalize(&protected_path)
                .expect("canonical protected file"),
            scope: PathScope::Exact,
        }];
        let result = scan_disclosure(&request(prompt_path, cwd, &[], &protected, &[]));
        assert!(matches!(result, Err(DisclosureError::ProtectedOverlap)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn noncanonical_grant_is_refused_before_overlap_checks() {
        let fixture = DisclosureFixture::new();
        let canonical_prompt = &fixture.prompt;
        // Keep the noncanonical grant explicit instead of relying on macOS's /tmp alias.
        let alias = fixture._temp.path().join("work-alias");
        std::os::unix::fs::symlink(&fixture.work, &alias).expect("work alias");
        let cwd = PathGrant {
            path: alias,
            scope: PathScope::Subtree,
        };
        assert_ne!(
            cwd.path,
            provenant_os::fs::canonicalize(&cwd.path).expect("canonical work root")
        );
        let result = scan_disclosure(&request(canonical_prompt, &cwd, &[], &[], &[]));
        assert!(matches!(result, Err(DisclosureError::InvalidPath)));
    }

    #[test]
    fn bounded_scan_refuses_secret_patterns_without_returning_content() {
        let fixture = DisclosureFixture::new();
        fs::write(fixture.work.join("notes.txt"), b"private token-abc-123").expect("file write");
        let result = scan_disclosure(&request(
            &fixture.prompt,
            &fixture.cwd,
            &[],
            &[],
            &[b"token-abc-123".to_vec()],
        ));
        assert!(matches!(result, Err(DisclosureError::SecretMatch)));
    }

    #[test]
    fn nonempty_secret_set_allows_files_with_ordinary_urls() {
        let fixture = DisclosureFixture::new();
        fs::write(
            fixture.work.join("notes.txt"),
            b"bearer values; Bearer values; bare sk-; Akia1234567890123456; read ssh://git@host and https://example.invalid/docs",
        )
        .expect("file write");
        let result = scan_disclosure(&request(
            &fixture.prompt,
            &fixture.cwd,
            &[],
            &[],
            &[b"secret-123".to_vec()],
        ));
        assert!(result.is_ok());
    }

    #[test]
    fn prompt_outside_read_roots_is_scanned_for_known_secrets() {
        let fixture = DisclosureFixture::new();
        fs::write(&fixture.prompt, b"prompt contains secret-123").expect("prompt file");
        let result = scan_disclosure(&request(
            &fixture.prompt,
            &fixture.cwd,
            &[],
            &[],
            &[b"secret-123".to_vec()],
        ));
        assert!(matches!(result, Err(DisclosureError::SecretMatch)));
    }

    #[test]
    fn direct_credential_root_is_rejected_before_disclosure() {
        let credential = PathGrant {
            path: PathBuf::from("/Users/alice/.ssh"),
            scope: PathScope::Subtree,
        };
        let prompt = Path::new("/tmp/prompt.txt");
        let cwd = PathGrant {
            path: PathBuf::from("/tmp/work-scoped"),
            scope: PathScope::Subtree,
        };
        let result = scan_disclosure(&request(prompt, &cwd, &[credential], &[], &[]));
        assert!(matches!(
            result,
            Err(DisclosureError::Grant(PermissionError::CredentialRoot))
        ));
    }

    #[test]
    fn oversized_sparse_file_refuses_before_reading_its_contents() {
        let fixture = DisclosureFixture::new();
        let file = fs::File::create(fixture.work.join("large.bin")).expect("file create");
        file.set_len(MAX_DISCLOSURE_BYTES + 1)
            .expect("sparse file size");
        let result = scan_disclosure(&request(&fixture.prompt, &fixture.cwd, &[], &[], &[]));
        assert!(matches!(result, Err(DisclosureError::BudgetExceeded)));
    }
}
