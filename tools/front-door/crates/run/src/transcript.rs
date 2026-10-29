//! The provider transcript an attempt left in its retained provider state.
//! The run records a pointer (adapter and session); the file stays with the
//! provider and is read in place, never copied.
use provenant_os::fs::{Directory, FileKind};
use provenant_records::run::{PathScope, WriteGrantRecord, WriteSource};
use std::{
    ffi::{OsStr, OsString},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// Largest tail of a transcript one read returns.
pub(crate) const WINDOW: u64 = 4 << 20;
const DEPTH: u8 = 8;
const ENTRIES: u32 = 20_000;

/// The `.jsonl` file named for `session`, as a directory handle, its name,
/// and a path relative to the attempt or absolute for an account transcript.
/// Bounded, never follows a link and stays on one device.
pub(crate) fn find(
    attempt: &Directory,
    session: &str,
    grants: &[WriteGrantRecord],
) -> Option<(Directory, OsString, PathBuf)> {
    fn walk(
        dir: &Directory,
        session: &str,
        depth: u8,
        device: u64,
        budget: &mut u32,
        path: &Path,
    ) -> Option<(Directory, OsString, PathBuf)> {
        let names: Vec<OsString> = dir.entries().ok()?.map_while(Result::ok).collect();
        for name in names {
            *budget = budget.checked_sub(1)?;
            let meta = dir.metadata_at(&name, false).ok()?;
            let text = name.to_string_lossy();
            if meta.kind == FileKind::Regular && text.ends_with(".jsonl") && text.contains(session)
            {
                return Some((dir.try_clone().ok()?, name.clone(), path.join(&name)));
            }
            if depth > 0 && meta.kind == FileKind::Directory && meta.device == device {
                let child = dir.open_verified_directory(&name).ok()?;
                if let Some(found) = walk(
                    &child,
                    session,
                    depth - 1,
                    device,
                    budget,
                    &path.join(&name),
                ) {
                    return Some(found);
                }
            }
        }
        None
    }
    if session.is_empty() {
        return None;
    }
    // Read-only Claude writes into the selected account. Its adapter records
    // the exact transcript state-write grant, so never search that account.
    for grant in grants {
        if grant.source != WriteSource::StateWrite || grant.scope != PathScope::Exact {
            continue;
        }
        let Ok(path) = grant.path.to_path_buf() else {
            continue;
        };
        let (Some(name), Some(parent)) = (path.file_name(), path.parent()) else {
            continue;
        };
        if name != OsStr::new(&format!("{session}.jsonl"))
            || parent.parent().and_then(Path::file_name) != Some(OsStr::new("projects"))
        {
            continue;
        }
        // Open each component beneath / without following symlinks; a stale
        // or linked grant falls through to the next candidate.
        let open = || {
            let mut dir = Directory::root(true).ok()?;
            for component in parent.strip_prefix("/").ok()?.components() {
                dir = dir.open_verified_directory(component.as_os_str()).ok()?;
            }
            (dir.metadata_at(name, false).ok()?.kind == FileKind::Regular).then_some(dir)
        };
        if let Some(dir) = open() {
            return Some((dir, name.to_owned(), path));
        }
    }
    let state = attempt
        .open_verified_directory(OsStr::new("provider-state"))
        .ok()?;
    let device = state.metadata().ok()?.device;
    walk(
        &state,
        session,
        DEPTH,
        device,
        &mut { ENTRIES },
        Path::new("provider-state"),
    )
}

/// Locate a provider-state transcript or a recorded account transcript.
pub fn locate(attempt: &Path, session: &str, grants: &[WriteGrantRecord]) -> Option<PathBuf> {
    let attempt = Directory::open_anchor(attempt).ok()?;
    find(&attempt, session, grants).map(|(_, _, path)| path)
}

/// The last `WINDOW` bytes of the transcript, starting at a line boundary, and
/// whether that is the whole file.
pub(crate) fn tail(dir: &Directory, name: &OsStr) -> Option<(Vec<u8>, bool)> {
    let mut file = dir.open_read(name).ok()?;
    let meta = file.metadata().ok()?;
    if meta.kind != FileKind::Regular {
        return None;
    }
    let clipped = meta.size > WINDOW;
    if clipped {
        file.seek(SeekFrom::Start(meta.size - WINDOW)).ok()?;
    }
    // A provider may append after metadata or seek. Read only the bytes the
    // metadata covered, so the result is that snapshot and `complete` holds.
    let mut bytes = Vec::new();
    file.take(meta.size.min(WINDOW))
        .read_to_end(&mut bytes)
        .ok()?;
    if clipped {
        let start = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1);
        bytes.drain(..start);
    }
    Some((bytes, !clipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn codex_rollout_path() -> TestResult {
        let temp = tempfile::tempdir()?;
        let path = Path::new(
            "provider-state/candidate-0/codex-home/sessions/2026/10/27/rollout-2026-10-27-sess-1848.jsonl",
        );
        std::fs::create_dir_all(temp.path().join(path.parent().ok_or("parent")?))?;
        std::fs::write(temp.path().join(path), "{}\n")?;
        assert_eq!(locate(temp.path(), "sess-1848", &[]), Some(path.into()));
        Ok(())
    }

    #[test]
    fn claude_account_path_and_symlinks() -> TestResult {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let project = root.join("account/projects/project-key");
        std::fs::create_dir_all(&project)?;
        let path = project.join("sess-1848.jsonl");
        std::fs::write(&path, "{}\n")?;
        let mut grant = WriteGrantRecord {
            extra: Default::default(),
            source: WriteSource::StateWrite,
            scope: PathScope::Exact,
            path: provenant_records::AbsolutePath::new(&path)?,
        };
        assert_eq!(
            locate(&root, "sess-1848", &[grant.clone()]),
            Some(path.clone())
        );
        let (dir, name, _) = find(
            &Directory::open_anchor(&root)?,
            "sess-1848",
            &[grant.clone()],
        )
        .ok_or("account transcript missing")?;
        assert_eq!(tail(&dir, &name), Some((b"{}\n".to_vec(), true)));
        assert!(locate(&root, "other-session", &[grant.clone()]).is_none());
        let link = root.join("linked-account");
        std::os::unix::fs::symlink(root.join("account"), &link)?;
        grant.path = provenant_records::AbsolutePath::new(
            &link.join("projects/project-key/sess-1848.jsonl"),
        )?;
        assert!(locate(&root, "sess-1848", &[grant.clone()]).is_none());
        let linked_file = project.join("linked-session.jsonl");
        std::os::unix::fs::symlink(project.join("sess-1848.jsonl"), &linked_file)?;
        grant.path = provenant_records::AbsolutePath::new(&linked_file)?;
        assert!(locate(&root, "linked-session", &[grant.clone()]).is_none());
        // A stale matching grant does not hide a later valid one.
        let mut stale = grant.clone();
        stale.path = provenant_records::AbsolutePath::new(
            &root.join("gone/projects/project-key/sess-1848.jsonl"),
        )?;
        grant.path = provenant_records::AbsolutePath::new(&path)?;
        assert_eq!(locate(&root, "sess-1848", &[stale, grant]), Some(path));
        Ok(())
    }

    #[test]
    fn transcript_larger_than_window_starts_at_a_line_boundary() -> TestResult {
        let temp = tempfile::tempdir()?;
        let mut bytes = vec![b'x'; usize::try_from(WINDOW)? + 10];
        bytes.extend_from_slice(b"\n{\"last\":true}\n");
        std::fs::write(temp.path().join("session.jsonl"), bytes)?;
        let dir = Directory::open_anchor(temp.path())?;
        assert_eq!(
            tail(&dir, OsStr::new("session.jsonl")),
            Some((b"{\"last\":true}\n".to_vec(), false))
        );
        Ok(())
    }
}
