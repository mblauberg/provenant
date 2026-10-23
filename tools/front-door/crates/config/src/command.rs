//! Managed command classification and race-detecting atomic publication.
use crate::{invalid, io, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

pub const MANAGED_MARKER: &[u8] =
    b"# Managed by scripts/install-harness; local edits to an installed copy are not preserved.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Absent,
    Existing,
    ManagedFile,
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    identity: (u64, u64, u32, u64, i64, i64),
    payload: Vec<u8>,
}

fn identity(m: &fs::Metadata) -> (u64, u64, u32, u64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.mode(),
        m.size(),
        m.mtime(),
        m.mtime_nsec(),
    )
}

fn snapshot(path: &Path) -> Result<Option<Snapshot>> {
    let before = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(path, e)),
    };
    let read = || -> std::io::Result<(Vec<u8>, fs::Metadata)> {
        let bytes = if before.is_symlink() {
            fs::read_link(path)?.as_os_str().as_bytes().to_vec()
        } else if before.is_file() {
            fs::read(path)?
        } else {
            vec![]
        };
        Ok((bytes, fs::symlink_metadata(path)?))
    };
    let changed = || Error::Collision {
        path: path.into(),
        reason: "changed during classification",
    };
    let (payload, after) = read().map_err(|e| {
        if matches!(
            e.kind(),
            std::io::ErrorKind::NotFound
                | std::io::ErrorKind::InvalidInput
                | std::io::ErrorKind::IsADirectory
                | std::io::ErrorKind::NotADirectory
        ) {
            changed()
        } else {
            io(path, e)
        }
    })?;
    if identity(&before) != identity(&after)
        || (before.ctime(), before.ctime_nsec()) != (after.ctime(), after.ctime_nsec())
    {
        return Err(changed());
    }
    Ok(Some(Snapshot {
        identity: identity(&before),
        payload,
    }))
}

fn owned_snapshot(source: &Path, destination: &Path) -> Result<(State, Option<Snapshot>)> {
    let Some(snapshot) = snapshot(destination)? else {
        return Ok((State::Absent, None));
    };
    let mode = snapshot.identity.2 & 0o170000;
    let reason = if mode == 0o120000 {
        "remove or repoint the link to continue"
    } else if mode != 0o100000 {
        "remove the file to continue"
    } else {
        if snapshot.payload == fs::read(source).map_err(|e| io(source, e))? {
            return Ok((State::Existing, Some(snapshot)));
        }
        // bytes.splitlines() recognises LF, CR and CRLF, including binary input.
        if snapshot
            .payload
            .split(|b| matches!(b, b'\n' | b'\r'))
            .any(|line| line == MANAGED_MARKER)
        {
            return Ok((State::ManagedFile, Some(snapshot)));
        }
        "remove or replace the file to continue"
    };
    Err(Error::Collision {
        path: destination.into(),
        reason,
    })
}

pub fn classify(source: &Path, destination: &Path) -> Result<State> {
    Ok(owned_snapshot(source, destination)?.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Published {
    Existing,
    Installed,
    Updated,
}

fn exchange(first: &Path, second: &Path) -> Result<()> {
    use provenant_supervisor::fs::Directory;
    let directory = first.parent().unwrap_or(Path::new("."));
    let anchor =
        Directory::open_anchor(&crate::resolve(directory)?).map_err(|e| io(directory, e.into()))?;
    let first_name = first
        .file_name()
        .ok_or_else(|| invalid("exchange source has no name"))?;
    let second_name = second
        .file_name()
        .ok_or_else(|| invalid("exchange destination has no name"))?;
    anchor
        .exchange(first_name, &anchor, second_name)
        .map_err(|e| io(second, e.into()))
}

fn restore(temporary: &Path, destination: &Path, prepared: &Option<Snapshot>) -> Result<bool> {
    if &snapshot(destination)? != prepared {
        return Ok(false);
    }
    exchange(temporary, destination)?;
    if &snapshot(temporary)? != prepared {
        exchange(temporary, destination)?;
        return Ok(false);
    }
    Ok(true)
}

fn stage(destination: &Path) -> Result<(PathBuf, fs::File)> {
    let parent = destination.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
    for _ in 0..128 {
        let suffix =
            provenant_supervisor::fs::temporary_name().map_err(|e| io(parent, e.into()))?;
        let temporary = parent.join(format!(".provenant.{}", suffix.to_string_lossy()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io(&temporary, e)),
        }
    }
    Err(invalid("could not reserve command staging file"))
}

pub fn publish(source: &Path, destination: &Path, expected: State) -> Result<Published> {
    let (current, original) = owned_snapshot(source, destination)?;
    if current != expected {
        return Err(invalid(format!(
            "provenant command collision={}; changed after preflight ({} -> {})",
            destination.display(),
            state_text(expected),
            state_text(current)
        )));
    }
    if current == State::Existing {
        return Ok(Published::Existing);
    }
    let (temporary, mut file) = stage(destination)?;
    let mut preserve = false;
    let result = (|| {
        file.write_all(&fs::read(source).map_err(|e| io(source, e))?)
            .map_err(|e| io(&temporary, e))?;
        file.sync_all().map_err(|e| io(&temporary, e))?;
        drop(file);
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))
            .map_err(|e| io(&temporary, e))?;
        let prepared = snapshot(&temporary)?;
        if current == State::Absent {
            fs::hard_link(&temporary, destination).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    invalid(format!(
                        "provenant command collision={}; created after preflight",
                        destination.display()
                    ))
                } else {
                    io(destination, e)
                }
            })?;
        } else {
            exchange(&temporary, destination)?;
            // Until displaced ownership is proven, even an unrelated read or rollback
            // failure must retain the displaced entry at its recovery path.
            preserve = true;
            let displaced = owned_snapshot(source, &temporary);
            let mismatch = match displaced {
                Ok((state, observed)) => state != current || observed != original,
                Err(Error::Collision { .. }) => true,
                Err(error) => return Err(error),
            };
            if mismatch {
                preserve = !restore(&temporary, destination, &prepared)?;
                let recovery = if preserve {
                    format!("; displaced path preserved={}", temporary.display())
                } else {
                    String::new()
                };
                return Err(invalid(format!(
                    "provenant command collision={}; changed during atomic publication{recovery}",
                    destination.display()
                )));
            }
            preserve = false;
        }
        let parent = destination.parent().unwrap_or(Path::new("."));
        provenant_supervisor::fs::Directory::open_anchor(&crate::resolve(parent)?)
            .and_then(|directory| directory.sync())
            .map_err(|e| io(parent, e.into()))?;
        Ok(if current == State::Absent {
            Published::Installed
        } else {
            Published::Updated
        })
    })();
    if !preserve {
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(&temporary, e)),
        }
    }
    result
}

fn state_text(state: State) -> &'static str {
    match state {
        State::Absent => "absent",
        State::Existing => "existing",
        State::ManagedFile => "managed-file",
    }
}

pub fn publish_expected(source: &Path, destination: &Path, expected: &str) -> Result<Published> {
    let state = match expected {
        "absent" => State::Absent,
        "existing" => State::Existing,
        "managed-file" => State::ManagedFile,
        _ => {
            return Err(invalid(format!(
                "invalid expected Provenant command state: {expected}"
            )))
        }
    };
    publish(source, destination, state)
}

/// CLI-edge input policy, retained separately from filesystem classification.
pub fn validate_paths(source: &Path, destination: &Path) -> Result<()> {
    if !source.is_absolute() {
        return Err(invalid("source must be absolute"));
    }
    if !destination.is_absolute() {
        return Err(invalid("destination must be absolute"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    #[test]
    fn binary_marker_requires_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::write(&source, b"new").unwrap();
        assert_eq!(classify(&source, &dest).unwrap(), State::Absent);
        fs::write(&dest, b"new").unwrap();
        assert_eq!(classify(&source, &dest).unwrap(), State::Existing);
        for newline in [b"\n".as_slice(), b"\r", b"\r\n"] {
            let bytes = [
                b"\0\xff".as_slice(),
                newline,
                MANAGED_MARKER,
                newline,
                b"binary",
            ]
            .concat();
            fs::write(&dest, bytes).unwrap();
            assert_eq!(classify(&source, &dest).unwrap(), State::ManagedFile);
        }
        fs::write(
            &dest,
            [b"prefix".as_slice(), MANAGED_MARKER, b"\n"].concat(),
        )
        .unwrap();
        assert!(classify(&source, &dest).is_err());
    }
    #[test]
    fn foreign_links_are_never_owned_even_if_identical() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::write(&source, b"same").unwrap();
        symlink(&source, &dest).unwrap();
        assert!(classify(&source, &dest)
            .unwrap_err()
            .to_string()
            .contains("remove or repoint"));
        fs::remove_file(source).unwrap();
        assert!(classify(&dir.path().join("source"), &dest).is_err());
    }
    #[test]
    fn directories_and_relative_paths_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(classify(&dir.path().join("source"), dir.path())
            .unwrap_err()
            .to_string()
            .contains("remove the file"));
        assert_eq!(
            validate_paths(Path::new("source"), dir.path())
                .unwrap_err()
                .to_string(),
            "source must be absolute"
        );
    }
    #[test]
    fn python_classification_differential() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::write(&source, b"new").unwrap();
        for bytes in [
            b"new".to_vec(),
            [b"\xff\n".as_slice(), MANAGED_MARKER, b"\n"].concat(),
            b"foreign".to_vec(),
        ] {
            fs::write(&dest, bytes).unwrap();
            let Some(output) = crate::oracle::python("scripts/install-provenant-command.py", "try: print(m.classify(p.parent/'source', p))\nexcept m.Collision: print('collision')", &dest) else { return; };
            let actual = match classify(&source, &dest) {
                Ok(State::Existing) => "existing",
                Ok(State::ManagedFile) => "managed-file",
                Ok(State::Absent) => "absent",
                Err(_) => "collision",
            };
            assert_eq!(String::from_utf8(output).unwrap().trim(), actual);
        }
    }
}

#[derive(Debug, clap::Parser)]
#[command(name = "install-provenant-command.py")]
pub struct Arguments {
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    Classify {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        destination: PathBuf,
    },
    Publish {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        expected: String,
    },
}

pub fn run(command: &Command) -> crate::codex::CommandResult {
    let result: Result<&str> = (|| match command {
        Command::Classify {
            source,
            destination,
        } => {
            validate_paths(source, destination)?;
            Ok(state_text(classify(source, destination)?))
        }
        Command::Publish {
            source,
            destination,
            expected,
        } => {
            validate_paths(source, destination)?;
            Ok(match publish_expected(source, destination, expected)? {
                Published::Existing => "existing",
                Published::Installed => "installed",
                Published::Updated => "updated",
            })
        }
    })();
    match result {
        Ok(output) => crate::codex::CommandResult {
            exit_code: 0,
            stdout: format!("{output}\n"),
            stderr: String::new(),
        },
        Err(error) => crate::codex::CommandResult {
            exit_code: 3,
            stdout: String::new(),
            stderr: format!("{error}\n"),
        },
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    #[test]
    fn binary_installs_updates_and_existing_does_not_chmod() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("bin/provenant");
        fs::write(&source, [MANAGED_MARKER, b"\nnew\0\xff"].concat()).unwrap();
        assert_eq!(
            publish(&source, &target, State::Absent).unwrap(),
            Published::Installed
        );
        assert_eq!(fs::read(&source).unwrap(), fs::read(&target).unwrap());
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o755);
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            publish(&source, &target, State::Existing).unwrap(),
            Published::Existing
        );
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o644);
        fs::write(&source, [MANAGED_MARKER, b"\nrebuilt"].concat()).unwrap();
        assert_eq!(
            publish(&source, &target, State::ManagedFile).unwrap(),
            Published::Updated
        );
        assert_eq!(fs::read(&target).unwrap(), fs::read(&source).unwrap());
        assert!(publish_expected(&source, &target, "foreign")
            .unwrap_err()
            .to_string()
            .contains("invalid expected"));
        assert!(publish(&source, &target, State::Absent)
            .unwrap_err()
            .to_string()
            .contains("changed after preflight (absent -> existing)"));
        assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 1);
    }
    #[test]
    fn rollback_restores_displaced_foreign_links_and_refuses_changed_destination() {
        let dir = tempfile::tempdir().unwrap();
        let temporary = dir.path().join("temporary");
        let destination = dir.path().join("destination");
        fs::write(&destination, b"prepared").unwrap();
        let prepared = snapshot(&destination).unwrap();
        std::os::unix::fs::symlink("foreign", &temporary).unwrap();
        assert!(restore(&temporary, &destination, &prepared).unwrap());
        assert_eq!(fs::read_link(&destination).unwrap(), Path::new("foreign"));
        fs::write(&temporary, b"displaced").unwrap();
        assert!(!restore(&temporary, &destination, &prepared).unwrap());
        assert_eq!(fs::read(&temporary).unwrap(), b"displaced");
    }
    #[test]
    fn publication_outputs_modes_and_refusals_match_python() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("dest");
        fs::write(&source, [MANAGED_MARKER, b"\nnew\0\xff"].concat()).unwrap();
        for expected in ["absent", "existing", "managed-file", "foreign"] {
            for content in [
                None,
                Some(b"foreign".to_vec()),
                Some([MANAGED_MARKER, b"\nold"].concat()),
            ] {
                let reset = || {
                    let _ = fs::remove_file(&target);
                    if let Some(bytes) = &content {
                        fs::write(&target, bytes).unwrap();
                    }
                };
                reset();
                let Some(old) = crate::oracle::python("scripts/install-provenant-command.py", &format!("import json\ntry: r=m.publish(p.parent/'source',p,{expected:?}); print(json.dumps([r,p.read_bytes().hex(),p.stat().st_mode & 0o777]))\nexcept m.Collision as e: print(json.dumps(str(e)))"),&target) else {return;};
                reset();
                let actual = match publish_expected(&source, &target, expected) {
                    Ok(outcome) => serde_json::json!([
                        match outcome {
                            Published::Existing => "existing",
                            Published::Installed => "installed",
                            Published::Updated => "updated",
                        },
                        crate::pending::hex(&fs::read(&target).unwrap()),
                        fs::metadata(&target).unwrap().mode() & 0o777
                    ]),
                    Err(e) => serde_json::json!(e.to_string()),
                };
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&old).unwrap(),
                    actual
                );
            }
        }
    }
}
