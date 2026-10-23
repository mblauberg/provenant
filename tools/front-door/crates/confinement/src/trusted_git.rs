//! Owner-selected Git locations and file-only grants for a writer's own branch.
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;

pub const PRIVATE_READ_ONLY: [&str; 3] = ["config.worktree", "commondir", "gitdir"];

#[derive(Debug, Error)]
pub enum GitLocationError {
    #[error("{0}")]
    Refused(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
fn io_error(path: &Path, source: io::Error) -> GitLocationError {
    GitLocationError::Io {
        path: path.to_owned(),
        source,
    }
}
fn read(path: &Path) -> Result<String, GitLocationError> {
    fs::read_to_string(path).map_err(|e| io_error(path, e))
}
fn resolve(path: &Path) -> Result<PathBuf, GitLocationError> {
    // Path.resolve(strict=False) permits missing private metadata at inspection time.
    fn inner(path: &Path, links: &mut usize) -> Result<PathBuf, GitLocationError> {
        let mut result = PathBuf::new();
        for part in path.components() {
            match part {
                Component::CurDir => (),
                Component::ParentDir => {
                    result.pop();
                }
                part => {
                    result.push(part.as_os_str());
                    match fs::symlink_metadata(&result) {
                        Ok(meta) if meta.file_type().is_symlink() => {
                            *links += 1;
                            if *links > 40 {
                                return Err(GitLocationError::Refused(
                                    "symlink loop in Git location".into(),
                                ));
                            }
                            let target =
                                fs::read_link(&result).map_err(|e| io_error(&result, e))?;
                            result.pop();
                            result = inner(&result.join(target), links)?;
                        }
                        Ok(_) => (),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                        Err(e) => return Err(io_error(&result, e)),
                    }
                }
            }
        }
        Ok(result)
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| io_error(path, e))?
            .join(path)
    };
    inner(&absolute, &mut 0)
}
fn symlink(path: &Path) -> bool {
    path.is_symlink()
}

pub fn writer_read_only(worktree: &Path, private: &Path, common: &Path) -> Vec<PathBuf> {
    let mut paths = vec![worktree.join(".git")];
    paths.extend(PRIVATE_READ_ONLY.map(|name| private.join(name)));
    paths.push(common.join("objects/info"));
    paths
}

pub fn writer_git_writes(
    private: &Path,
    common: &Path,
    reference: Option<&str>,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let files = reference
        .filter(|r| !r.is_empty())
        .map(|r| {
            vec![
                common.join(r),
                common.join(format!("{r}.lock")),
                common.join("logs").join(r),
            ]
        })
        .unwrap_or_default();
    (vec![private.to_owned(), common.join("objects")], files)
}

pub fn prepare_writer_ref(common: &Path, reference: &str) -> Result<(), GitLocationError> {
    for file in [common.join(reference), common.join("logs").join(reference)] {
        let parent = file
            .parent()
            .ok_or_else(|| GitLocationError::Refused("writer ref has no parent".into()))?;
        let relative = parent.strip_prefix(common).map_err(|_| {
            GitLocationError::Refused("writer ref is outside common Git directory".into())
        })?;
        let mut path = common.to_owned();
        for part in relative.components() {
            path.push(part);
            if symlink(&path) {
                return Err(GitLocationError::Refused(format!(
                    "writer ref directory {} is a link",
                    path.display()
                )));
            }
            match fs::create_dir(&path) {
                Ok(()) => (),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => (),
                Err(e) => return Err(io_error(&path, e)),
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitLocation {
    pub worktree: PathBuf,
    pub private: PathBuf,
    pub common: PathBuf,
}

impl GitLocation {
    pub fn validate(&self) -> Result<(), GitLocationError> {
        let gitfile = self.worktree.join(".git");
        if self.private == self.common {
            if symlink(&gitfile) || resolve(&gitfile)? != self.common {
                return Err(GitLocationError::Refused(
                    "worktree Git directory differs from owner-recorded location".into(),
                ));
            }
        } else {
            if symlink(&gitfile)
                || read(&gitfile)?.trim() != format!("gitdir: {}", self.private.display())
            {
                return Err(GitLocationError::Refused(
                    "worktree gitfile differs from owner-recorded location".into(),
                ));
            }
            for name in PRIVATE_READ_ONLY {
                if symlink(&self.private.join(name)) {
                    return Err(GitLocationError::Refused(format!(
                        "writer Git {name} is a link"
                    )));
                }
            }
            let common = resolve(
                &self
                    .private
                    .join(read(&self.private.join("commondir"))?.trim()),
            )?;
            let back = resolve(Path::new(read(&self.private.join("gitdir"))?.trim()))?;
            if common != self.common || back != gitfile {
                return Err(GitLocationError::Refused(
                    "discovered common Git directory differs from owner-recorded location".into(),
                ));
            }
        }
        if fs::symlink_metadata(self.common.join("objects/info/alternates")).is_ok() {
            return Err(GitLocationError::Refused(
                "alternate object stores cannot vouch for instruction protection".into(),
            ));
        }
        Ok(())
    }

    pub fn writer_ref(&self) -> Result<Option<String>, GitLocationError> {
        let head = read(&self.private.join("HEAD"))?;
        let head = head.trim();
        let Some(reference) = head.strip_prefix("ref: ") else {
            return Ok(None);
        };
        if !reference.starts_with("refs/heads/") || !check_ref_format(reference)? {
            return Err(GitLocationError::Refused(
                "writer HEAD must name a local branch or be detached".into(),
            ));
        }
        let mut others = vec![self.common.join("HEAD")];
        for path in registrations(&self.common)? {
            if path != self.private {
                others.push(path.join("HEAD"));
            }
        }
        for path in others {
            if path.is_file() && read(&path)?.trim() == head {
                return Err(GitLocationError::Refused(format!(
                    "writer branch {reference} is checked out by another worktree"
                )));
            }
        }
        Ok(Some(reference.into()))
    }
}

/// Match the source's five-second Git subprocess deadline. This child is only a
/// local ref parser, never a provider or a lane; it has no descendants to retain.
fn check_ref_format(reference: &str) -> Result<bool, GitLocationError> {
    let mut child = Command::new("git")
        .args(["check-ref-format", reference])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| io_error(Path::new("git"), e))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            result => {
                // Always reap this owned parser, including the wait-error path.
                let _ = child.kill();
                let reap_deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < reap_deadline {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        break;
                    }
                    std::thread::yield_now();
                }
                return Err(io_error(
                    Path::new("git"),
                    match result {
                        Err(e) => e,
                        _ => io::Error::new(
                            io::ErrorKind::TimedOut,
                            "git check-ref-format timed out after 5 seconds",
                        ),
                    },
                ));
            }
        }
    }
}

fn registrations(common: &Path) -> Result<Vec<PathBuf>, GitLocationError> {
    let path = common.join("worktrees");
    match fs::read_dir(&path) {
        Ok(entries) => Ok(entries.filter_map(Result::ok).map(|e| e.path()).collect()),
        // Python's Path.glob yields no matches when worktrees is absent, unreadable or not a dir.
        Err(_) => Ok(Vec::new()),
    }
}

pub fn owner_location(cwd: &Path, common: Option<&Path>) -> Result<GitLocation, GitLocationError> {
    let worktree = resolve(cwd)?;
    let gitfile = worktree.join(".git");
    let common = match common {
        Some(path) => resolve(path)?,
        None if gitfile.is_dir() && !symlink(&gitfile) => gitfile.clone(),
        None => {
            if symlink(&gitfile) || !gitfile.is_file() {
                return Err(GitLocationError::Refused("not a Git checkout".into()));
            }
            let contents = read(&gitfile)?;
            let contents = contents.trim();
            let private = worktree.join(contents.strip_prefix("gitdir: ").unwrap_or(contents));
            resolve(&private.join(read(&private.join("commondir"))?.trim()))?
        }
    };
    if common == gitfile {
        return Ok(GitLocation {
            worktree,
            private: common.clone(),
            common,
        });
    }
    for private in registrations(&common)? {
        let back = private.join("gitdir");
        if back.is_file() && !symlink(&back) && Path::new(read(&back)?.trim()) == gitfile {
            return Ok(GitLocation {
                worktree,
                private,
                common,
            });
        }
    }
    Err(GitLocationError::Refused(
        "writer is not registered in the owner common Git directory".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fixture() -> (tempfile::TempDir, GitLocation) {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let worktree = root.join("lane");
        let common = root.join("repo/.git");
        let private = common.join("worktrees/lane");
        fs::create_dir_all(&worktree).unwrap();
        fs::create_dir_all(&private).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", private.display()),
        )
        .unwrap();
        fs::write(private.join("commondir"), "../..\n").unwrap();
        fs::write(
            private.join("gitdir"),
            format!("{}\n", worktree.join(".git").display()),
        )
        .unwrap();
        fs::write(private.join("HEAD"), "ref: refs/heads/lane\n").unwrap();
        fs::write(common.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        (
            tmp,
            GitLocation {
                worktree,
                private,
                common,
            },
        )
    }

    #[test]
    fn owner_registration_and_identity_are_required() {
        let (_tmp, location) = fixture();
        assert_eq!(owner_location(&location.worktree, None).unwrap(), location);
        assert_eq!(
            owner_location(&location.worktree, Some(&location.common)).unwrap(),
            location
        );
        location.validate().unwrap();
        fs::write(location.private.join("gitdir"), "/elsewhere/.git").unwrap();
        assert!(location.validate().is_err());
        assert!(owner_location(&location.worktree, Some(&location.common)).is_err());
    }

    #[test]
    fn git_links_and_even_dangling_alternates_are_refused() {
        for name in PRIVATE_READ_ONLY {
            let (_tmp, location) = fixture();
            let file = location.private.join(name);
            let _ = fs::remove_file(&file);
            symlink("missing", file).unwrap();
            assert!(
                location
                    .validate()
                    .unwrap_err()
                    .to_string()
                    .contains("is a link")
            );
        }
        let (_tmp, location) = fixture();
        fs::create_dir_all(location.common.join("objects/info")).unwrap();
        symlink("missing", location.common.join("objects/info/alternates")).unwrap();
        assert!(
            location
                .validate()
                .unwrap_err()
                .to_string()
                .starts_with("alternate object stores")
        );
    }

    #[test]
    fn writer_cannot_take_primary_or_sibling_branch() {
        let (_tmp, location) = fixture();
        assert_eq!(
            location.writer_ref().unwrap(),
            Some("refs/heads/lane".into())
        );
        fs::write(location.private.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(location.writer_ref().is_err());
        fs::write(location.private.join("HEAD"), "ref: refs/heads/lane\n").unwrap();
        let sibling = location.common.join("worktrees/other");
        fs::create_dir_all(&sibling).unwrap();
        fs::write(sibling.join("HEAD"), "ref: refs/heads/lane\n").unwrap();
        assert!(location.writer_ref().is_err());
        fs::write(location.private.join("HEAD"), "a".repeat(40)).unwrap();
        assert_eq!(location.writer_ref().unwrap(), None);
    }

    #[test]
    fn grants_do_not_include_shared_refs_or_instruction_roots() {
        let (_tmp, location) = fixture();
        let (dirs, files) =
            writer_git_writes(&location.private, &location.common, Some("refs/heads/lane"));
        assert_eq!(
            dirs,
            [location.private.clone(), location.common.join("objects")]
        );
        assert_eq!(
            files,
            [
                location.common.join("refs/heads/lane"),
                location.common.join("refs/heads/lane.lock"),
                location.common.join("logs/refs/heads/lane")
            ]
        );
        assert!(
            writer_git_writes(&location.private, &location.common, None)
                .1
                .is_empty()
        );
        assert_eq!(
            writer_read_only(&location.worktree, &location.private, &location.common).len(),
            5
        );
    }

    #[test]
    fn ref_preparation_rejects_linked_parent() {
        let (_tmp, location) = fixture();
        prepare_writer_ref(&location.common, "refs/heads/lane").unwrap();
        fs::remove_dir(location.common.join("refs/heads")).unwrap();
        symlink(
            location.common.join("logs"),
            location.common.join("refs/heads"),
        )
        .unwrap();
        assert!(
            prepare_writer_ref(&location.common, "refs/heads/lane")
                .unwrap_err()
                .to_string()
                .ends_with("is a link")
        );
    }

    #[test]
    fn discovery_records_and_grants_match_python() {
        let (_tmp, location) = fixture();
        let Some(actual) = crate::python::oracle(
            r#"
import trusted_git
from pathlib import Path
loc=trusted_git.owner_location(fixture['worktree'])
loc.validate()
ref=loc.writer_ref()
dirs, files=trusted_git.writer_git_writes(loc.private, loc.common, ref)
print(json.dumps({'record': loc.record(), 'ref': ref, 'dirs': list(map(str, dirs)),
                  'files': list(map(str, files)),
                  'readonly': list(map(str, trusted_git.writer_read_only(loc.worktree, loc.private, loc.common)))}))
"#,
            &serde_json::to_value(&location).unwrap(),
        ) else {
            return;
        };
        let reference = location.writer_ref().unwrap();
        let (dirs, files) =
            writer_git_writes(&location.private, &location.common, reference.as_deref());
        assert_eq!(
            actual,
            serde_json::json!({"record": location, "ref": reference, "dirs": dirs,
            "files": files, "readonly": writer_read_only(&location.worktree, &location.private, &location.common)})
        );
    }

    #[test]
    fn non_directory_worktrees_error_path_matches_python_glob() {
        let (_tmp, location) = fixture();
        fs::remove_dir_all(location.common.join("worktrees")).unwrap();
        fs::write(location.common.join("worktrees"), "not a directory").unwrap();
        let Some(expected) = crate::python::oracle(
            r#"
import trusted_git
try:
    trusted_git.owner_location(fixture['worktree'], common=fixture['common'])
except ValueError as exc:
    print(json.dumps(str(exc)))
else:
    raise AssertionError('fixture unexpectedly registered')
"#,
            &serde_json::json!({
                "worktree": location.worktree,
                "common": location.common,
            }),
        ) else {
            return;
        };
        assert_eq!(
            serde_json::to_value(
                owner_location(&location.worktree, Some(&location.common))
                    .unwrap_err()
                    .to_string()
            )
            .unwrap(),
            expected
        );
    }
}
