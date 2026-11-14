//! Recoverable explicit discard and its retention owner. No link is followed.
use super::*;
use provenant_os::fs::{FileHandle, FileKind, PayloadTree, PublishMode, temporary_name};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

#[derive(Debug, thiserror::Error)]
#[error("archive {operation} at {path:?}: {cause}")]
pub struct ArchiveError {
    pub operation: &'static str,
    pub path: PathBuf,
    #[source]
    pub cause: io::Error,
}
#[derive(Clone, Debug, Serialize)]
pub struct ArchiveInfo {
    pub path: PathBuf,
    pub tip: GitOid,
}
#[derive(Debug, Serialize)]
pub struct ArchiveIssue {
    pub path: PathBuf,
    pub operation: String,
    pub cause: String,
}
#[derive(Debug, Default, Serialize)]
pub struct ArchiveReport {
    pub removed: Vec<PathBuf>,
    pub bytes: u64,
    pub errors: Vec<ArchiveIssue>,
}

fn failure(operation: &'static str, path: &Path, cause: impl std::fmt::Display) -> ArchiveError {
    ArchiveError {
        operation,
        path: path.into(),
        cause: io::Error::other(cause.to_string()),
    }
}

pub(super) fn capture(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    tip: GitOid,
    cancel: &CancelToken,
) -> Result<ArchiveInfo, WorktreeError> {
    let root_name = OsStr::new("discard-archives");
    match context.project_state.mkdir(root_name, 0o700) {
        Ok(()) => {}
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    let root = context.project_state.open_verified_directory(root_name)?;
    let nonce = temporary_name()?;
    let stage_name = OsString::from(format!(".stage-{}", nonce.to_string_lossy()));
    root.mkdir(&stage_name, 0o700)?;
    let stage = root.open_verified_directory(&stage_name)?;
    let stage_path = stage.anchored_path();
    // An incomplete stage is never evidence authorising discard.
    let capture = (|| -> Result<(), WorktreeError> {
        let reference = GitRefName::new(format!(
            "refs/provenant/discard-archives/{}",
            nonce.to_string_lossy()
        ))
        .map_err(|_| WorktreeError::InvalidRequest)?;
        execute_mutation(
            context,
            GitMutation::CreateArchiveRef {
                reference: reference.clone(),
                tip,
            },
            cancel,
        )?;
        execute_mutation(
            context,
            GitMutation::BundleCreate {
                output: stage_path.join("tip.bundle"),
                reference: reference.clone(),
            },
            cancel,
        )?;
        execute_mutation(
            context,
            GitMutation::DeleteRef {
                name: reference,
                expected_old_oid: tip,
            },
            cancel,
        )?;
        let bundle = stage.open_read(OsStr::new("tip.bundle"))?;
        bundle.sync()?;
        for (name, operation) in [
            ("worktree.patch", GitRead::DirtyPatch),
            ("index.patch", GitRead::IndexPatch),
            ("status.porcelain-v2.z", GitRead::StatusIncludingIgnored),
        ] {
            let result = run_git(context, repo, operation, cancel)?;
            if exit_code(&result)? != 0
                || result.stdout.truncated
                || result.drain_truncated
                || result.stdout.read_failed
                || result.stdout.sink_failed
            {
                return Err(WorktreeError::InvalidOutput);
            }
            stage
                .atomic_write(
                    OsStr::new(name),
                    ".archive-",
                    &result.stdout.bytes,
                    PublishMode::Replace,
                )?
                .require_durable()
                .map_err(|e| failure("publish", &stage_path.join(name), e))?;
        }
        let paths = run_git(context, repo, GitRead::Untracked, cancel)?;
        if exit_code(&paths)? != 0
            || paths.stdout.truncated
            || paths.drain_truncated
            || paths.stdout.read_failed
            || paths.stdout.sink_failed
        {
            return Err(WorktreeError::InvalidOutput);
        }
        let mut tar = stage.create_new(OsStr::new("untracked.tar"))?;
        let source = Directory::open_anchor(repo.root_path())?;
        for path in paths
            .stdout
            .bytes
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
        {
            let path = PathBuf::from(OsString::from_vec(path.to_vec()));
            append_path(&source, &path, &mut tar)
                .map_err(|e| failure("archive untracked", &repo.root_path().join(&path), e))?;
        }
        tar.write_all(&[0; 1024])
            .map_err(|e| failure("finish tar", &stage_path, e))?;
        tar.sync()?;
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": 2, "tip": tip, "project": context.binding.project_id(),
            "worktree": repo.root_path(), "created_at": context.clock.wall_now().map_err(|e| failure("clock", &stage_path, e))?.as_micros(),
            "format": "git-bundle, binary worktree/index patches, porcelain-v2-z, ustar"
        })).map_err(|e| failure("manifest", &stage_path, e))?;
        stage
            .atomic_write(
                OsStr::new("manifest.json"),
                ".archive-",
                &manifest,
                PublishMode::Replace,
            )?
            .require_durable()
            .map_err(|e| failure("publish manifest", &stage_path, e))?;
        stage.sync()?;
        Ok(())
    })();
    capture?;
    let name = OsString::from(format!("archive-{}", nonce.to_string_lossy()));
    root.rename_noreplace(&stage_name, &root, &name)?;
    root.sync()?;
    Ok(ArchiveInfo {
        path: root.anchored_path().join(name),
        tip,
    })
}

/// POSIX ustar, deliberately refusing an unrepresentable path before discard.
/// Ancestors are opened through held directory descriptors; symlinks are data.
fn append_path(root: &Directory, path: &Path, out: &mut FileHandle) -> io::Result<()> {
    let components: Vec<_> = path.components().collect();
    if components.is_empty()
        || components
            .iter()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
    {
        return Err(io::Error::other("unsafe repository-relative archive path"));
    }
    let mut directory = Directory::open_anchor(&root.anchored_path()).map_err(io::Error::other)?;
    for component in &components[..components.len() - 1] {
        directory = directory
            .open_verified_directory(component.as_os_str())
            .map_err(io::Error::other)?;
    }
    let name = components
        .last()
        .ok_or_else(|| io::Error::other("missing archive name"))?
        .as_os_str();
    let metadata = directory
        .metadata_at(name, false)
        .map_err(io::Error::other)?;
    let mut header = [0_u8; 512];
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() <= 100 {
        header[..bytes.len()].copy_from_slice(bytes);
    } else {
        let split = bytes
            .iter()
            .enumerate()
            .rev()
            .find(|(i, b)| **b == b'/' && *i <= 155 && bytes.len() - *i - 1 <= 100)
            .map(|(i, _)| i)
            .ok_or_else(|| io::Error::other("path exceeds ustar name/prefix capacity"))?;
        header[..bytes.len() - split - 1].copy_from_slice(&bytes[split + 1..]);
        header[345..345 + split].copy_from_slice(&bytes[..split]);
    }
    fn octal(field: &mut [u8], value: u64) -> io::Result<()> {
        let value = format!("{value:0width$o}", width = field.len() - 1);
        if value.len() >= field.len() {
            return Err(io::Error::other("ustar numeric overflow"));
        }
        field[..value.len()].copy_from_slice(value.as_bytes());
        Ok(())
    }
    octal(&mut header[100..108], u64::from(metadata.mode & 0o777))?;
    octal(&mut header[108..116], u64::from(metadata.uid))?;
    octal(&mut header[116..124], 0)?;
    let size = if metadata.kind == FileKind::Regular {
        metadata.size
    } else {
        0
    };
    octal(&mut header[124..136], size)?;
    octal(
        &mut header[136..148],
        (metadata.modified_ns.max(0) / 1_000_000_000) as u64,
    )?;
    header[148..156].fill(b' ');
    header[156] = match metadata.kind {
        FileKind::Regular => b'0',
        FileKind::Symlink => {
            let target = directory.read_link(name).map_err(io::Error::other)?;
            let target = target.as_os_str().as_bytes();
            if target.len() > 100 {
                return Err(io::Error::other("symlink exceeds ustar capacity"));
            }
            header[157..157 + target.len()].copy_from_slice(target);
            b'2'
        }
        _ => return Err(io::Error::other("untracked entry is not a file or link")),
    };
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum: u64 = header.iter().map(|b| u64::from(*b)).sum();
    octal(&mut header[148..155], checksum)?;
    header[155] = b' ';
    out.write_all(&header)?;
    if metadata.kind == FileKind::Regular {
        let mut file = directory.open_read(name).map_err(io::Error::other)?;
        if !metadata.unchanged(&file.metadata().map_err(io::Error::other)?) {
            return Err(io::Error::other("untracked file moved"));
        }
        let copied = io::copy(&mut file.by_ref().take(size), out)?;
        if copied != size || !metadata.unchanged(&file.metadata().map_err(io::Error::other)?) {
            return Err(io::Error::other("untracked file changed during archive"));
        }
        out.write_all(&vec![0; ((512 - size % 512) % 512) as usize])?;
    }
    Ok(())
}

pub fn collect_archives(
    project_state: &Directory,
    before_ns: i128,
    dry_run: bool,
) -> ArchiveReport {
    let mut report = ArchiveReport::default();
    let root = match project_state.open_verified_directory(OsStr::new("discard-archives")) {
        Ok(root) => root,
        Err(error) if error.is_not_found() => return report,
        Err(error) => {
            report.errors.push(ArchiveIssue {
                path: project_state.anchored_path(),
                operation: "open archives".into(),
                cause: error.to_string(),
            });
            return report;
        }
    };
    let collect = (|| -> Result<(), FsError> {
        for entry in root.entries()? {
            let name = entry?;
            if !name.as_bytes().starts_with(b"archive-") {
                continue;
            }
            let metadata = root.metadata_at(&name, false)?;
            if metadata.kind != FileKind::Directory || metadata.modified_ns >= before_ns {
                continue;
            }
            let path = root.anchored_path().join(&name);
            let tree = PayloadTree::claim(&root, &name, &metadata)?;
            fn visit(tree: &PayloadTree, dry: bool) -> Result<u64, FsError> {
                let mut bytes = 0_u64;
                for name in tree.entries()? {
                    let name = name?;
                    let metadata = tree.metadata_at(&name)?;
                    bytes = bytes.saturating_add(metadata.allocated);
                    if metadata.kind == FileKind::Directory {
                        bytes = bytes.saturating_add(visit(&tree.child(&name)?, dry)?);
                        if !dry {
                            tree.rmdir(&name)?;
                        }
                    } else if !dry {
                        tree.unlink(&name)?;
                    }
                }
                Ok(bytes)
            }
            let bytes = visit(&tree, dry_run)?;
            if !dry_run {
                root.rmdir(&name)?;
                root.sync()?;
            }
            report.bytes = report.bytes.saturating_add(bytes);
            report.removed.push(path);
        }
        Ok(())
    })();
    if let Err(error) = collect {
        report.errors.push(ArchiveIssue {
            path: root.anchored_path(),
            operation: "collect archives".into(),
            cause: error.to_string(),
        });
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn untracked_tar_preserves_links_and_never_reads_their_targets()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::fs::write(outside.path().join("secret"), b"outside sentinel")?;
        std::fs::create_dir(temp.path().join("nested"))?;
        std::fs::write(temp.path().join("nested/file"), b"owned payload")?;
        std::os::unix::fs::symlink(outside.path().join("secret"), temp.path().join("alias"))?;
        let root = Directory::open_anchor(temp.path())?;
        let mut out = root.create_new(OsStr::new("fixture.tar"))?;
        append_path(&root, Path::new("nested/file"), &mut out)?;
        append_path(&root, Path::new("alias"), &mut out)?;
        out.sync()?;
        let bytes = std::fs::read(temp.path().join("fixture.tar"))?;
        assert!(
            bytes
                .windows(b"owned payload".len())
                .any(|b| b == b"owned payload")
        );
        assert!(
            !bytes
                .windows(b"outside sentinel".len())
                .any(|b| b == b"outside sentinel")
        );
        assert_eq!(bytes[1024 + 156], b'2');
        Ok(())
    }
}
