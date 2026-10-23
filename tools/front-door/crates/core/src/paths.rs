//! Path expansion uses explicit home-directory observations supplied by the caller.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Default, Clone)]
pub struct Homes {
    pub current: Option<PathBuf>,
    pub named: BTreeMap<OsString, PathBuf>,
}
#[derive(Debug, Error)]
pub enum PathError {
    #[error("Could not determine home directory")]
    Home,
    #[error("lstat: embedded null character in path")]
    Null { path: PathBuf },
    #[error("cannot resolve path: {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

// Pathlib collapses repeated separators and dot components when constructing a
// path, while preserving a POSIX double-slash root and parent components.
pub(crate) fn lexical_path(path: &Path) -> PathBuf {
    let mut normal: PathBuf = path
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect();
    if normal.as_os_str().is_empty() {
        normal.push(".");
    }
    let raw = path.as_os_str().as_encoded_bytes();
    if raw.starts_with(b"//") && !raw.starts_with(b"///") {
        let tail = normal.strip_prefix("/").unwrap_or(&normal);
        let mut double = PathBuf::from("//");
        double.push(tail);
        normal = double;
    }
    normal
}

pub fn expand_user(path: &Path, homes: &Homes) -> Result<PathBuf, PathError> {
    let path = lexical_path(path);
    let Some(Component::Normal(first)) = path.components().next() else {
        return Ok(path);
    };
    let Some(name) = first.as_encoded_bytes().strip_prefix(b"~") else {
        return Ok(path);
    };
    let home = if name.is_empty() {
        homes.current.as_ref()
    } else {
        homes
            .named
            .iter()
            .find(|(key, _)| key.as_encoded_bytes() == name)
            .map(|(_, value)| value)
    }
    .ok_or(PathError::Home)?;
    let mut tail = path.components();
    tail.next();
    Ok(lexical_path(&home.join(tail.as_path())))
}

/// Python Path.resolve(strict=False): resolve links even before a missing tail,
/// and process parent components only after following a link.
pub fn resolve(path: &Path) -> Result<PathBuf, PathError> {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(PathError::Null { path: path.into() });
    }
    let absolute = if path.is_absolute() {
        path.into()
    } else {
        std::env::current_dir()
            .map_err(|source| PathError::Io {
                path: path.into(),
                source,
            })?
            .join(path)
    };
    resolve_inner(&absolute)
}
fn resolve_inner(path: &Path) -> Result<PathBuf, PathError> {
    enum Part {
        Name(OsString),
        EndLink(PathBuf),
    }
    let mut pending: VecDeque<_> = path
        .components()
        .map(|c| Part::Name(c.as_os_str().into()))
        .collect();
    let mut links = BTreeSet::new();
    let mut result = PathBuf::new();
    while let Some(part) = pending.pop_front() {
        let name = match part {
            Part::EndLink(link) => {
                links.remove(&link);
                continue;
            }
            Part::Name(name) => name,
        };
        if name == "." {
            continue;
        }
        if name == ".." {
            result.pop();
            continue;
        }
        result.push(&name);
        // Non-strict resolution preserves nonexistent/inaccessible tails.
        if let Ok(link) = fs::read_link(&result) {
            if !links.insert(result.clone()) {
                return Err(PathError::Io {
                    path: result,
                    source: io::Error::other("Symlink loop"),
                });
            }
            pending.push_front(Part::EndLink(result.clone()));
            result.pop();
            for component in link.components().rev() {
                pending.push_front(Part::Name(component.as_os_str().into()));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn home_expansion_is_explicit_and_keeps_relative_and_symlink_paths() {
        let homes = Homes {
            current: Some("/home/me".into()),
            named: BTreeMap::from([("other".into(), "/home/other".into())]),
        };
        assert_eq!(
            expand_user(Path::new("~/tree"), &homes).unwrap(),
            Path::new("/home/me/tree")
        );
        assert_eq!(
            expand_user(Path::new("~other/tree"), &homes).unwrap(),
            Path::new("/home/other/tree")
        );
        assert!(expand_user(Path::new("~unknown"), &homes).is_err());
        assert_eq!(
            expand_user(Path::new("relative/./tree"), &homes).unwrap(),
            Path::new("relative/tree")
        );
    }
    #[cfg(unix)]
    #[test]
    fn resolve_follows_links_before_parent_components_and_detects_cycles() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("real/deep")).unwrap();
        symlink("real/deep", dir.path().join("link")).unwrap();
        assert_eq!(
            resolve(&dir.path().join("link/../new")).unwrap(),
            fs::canonicalize(dir.path()).unwrap().join("real/new")
        );
        symlink("loop", dir.path().join("loop")).unwrap();
        assert!(resolve(&dir.path().join("loop")).is_err());
    }
}

#[cfg(test)]
mod lexical_tests {
    use super::*;
    #[test]
    fn pathlib_spelling_and_null_rejection() {
        for (raw, expected) in [
            ("./a//b/", "a/b"),
            ("", "."),
            ("./", "."),
            ("//a//b/", "//a/b"),
            ("///a/./b", "/a/b"),
        ] {
            assert_eq!(
                lexical_path(Path::new(raw)).as_os_str(),
                Path::new(expected).as_os_str()
            );
        }
        assert!(matches!(
            resolve(Path::new("allowed\0/x")),
            Err(PathError::Null { .. })
        ));
    }
}

#[cfg(all(test, unix))]
mod long_chain_tests {
    use super::*;
    #[test]
    fn long_acyclic_link_chains_do_not_use_call_stack() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..512 {
            std::os::unix::fs::symlink(
                format!("link-{}", n + 1),
                dir.path().join(format!("link-{n}")),
            )
            .unwrap();
        }
        assert_eq!(
            resolve(&dir.path().join("link-0/tail")).unwrap(),
            fs::canonicalize(dir.path()).unwrap().join("link-512/tail")
        );
    }
}
