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
    #[error("path is not valid UTF-8: {path}")]
    Encoding { path: PathBuf },
    #[error("Could not determine home directory.")]
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
/// and process parent components only after following a link. As in CPython
/// >=3.13, a symlink loop is retained rather than an error in non-strict mode.
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
    Ok(resolve_inner(&absolute))
}
fn resolve_inner(path: &Path) -> PathBuf {
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
                // CPython >=3.13 non-strict realpath keeps the unresolved
                // component on ELOOP. Strict callers use `realpath` instead.
                continue;
            }
            pending.push_front(Part::EndLink(result.clone()));
            result.pop();
            for component in link.components().rev() {
                pending.push_front(Part::Name(component.as_os_str().into()));
            }
        }
    }
    result
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
    fn resolve_follows_links_before_parent_components_and_retains_non_strict_cycles() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("real/deep")).unwrap();
        symlink("real/deep", dir.path().join("link")).unwrap();
        assert_eq!(
            resolve(&dir.path().join("link/../new")).unwrap(),
            fs::canonicalize(dir.path()).unwrap().join("real/new")
        );
        symlink("loop", dir.path().join("loop")).unwrap();
        assert_eq!(
            resolve(&dir.path().join("loop")).unwrap(),
            fs::canonicalize(dir.path()).unwrap().join("loop")
        );
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

#[cfg(all(test, unix))]
mod shared_helper_tests {
    use super::*;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    #[test]
    fn path_order_matches_python_surrogateescape_not_lossy_or_utf16() {
        let fixtures = vec![
            b"abc".to_vec(),
            b"a\xff".to_vec(),
            "😀".as_bytes().to_vec(),
            "\u{e000}".as_bytes().to_vec(),
            b"\xed\xb2\x80".to_vec(),
        ];
        let Some(expected) = crate::test_support::python_values(
            "print(json.dumps([[ord(c) for c in os.fsdecode(bytes(v))] for v in values]))",
            &serde_json::json!(fixtures),
        ) else {
            return;
        };
        let actual: Vec<_> = fixtures
            .iter()
            .map(|v| path_sort_key(std::ffi::OsStr::from_bytes(v)))
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
        assert!(matches!(
            path_text(Path::new(std::ffi::OsStr::from_bytes(b"x\xff"))),
            Err(PathError::Encoding { .. })
        ));
    }
    #[test]
    fn resolve_and_realpath_match_python_links_and_missing_tails() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("real/deep")).unwrap();
        fs::write(dir.path().join("file"), "x").unwrap();
        std::os::unix::fs::symlink("real/deep", dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink("loop-b", dir.path().join("loop-a")).unwrap();
        std::os::unix::fs::symlink("loop-a", dir.path().join("loop-b")).unwrap();
        let paths = [
            "loop-a",
            "loop-b/x",
            "loop-a/../new",
            "link/../new",
            "link/../deep",
            "missing/../tail",
            "file/tail",
            "./link",
            "link/../../file",
        ];
        let input = dir.path().join("input.json");
        fs::write(&input, serde_json::to_vec(&paths).unwrap()).unwrap();
        let Some(expected) = crate::test_support::python(
            "",
            r#"
if sys.version_info < (3,13):
    print('Python >=3.13 required for non-strict symlink-loop semantics', file=sys.stderr); sys.exit(77)
base=pathlib.Path(sys.argv[3]); out=[]
for v in json.loads((base/'input.json').read_text()):
    p=base/v; row=[]
    for strict in [False, True]:
        try: row.append(str(p.resolve(strict=strict)))
        except OSError: row.append(None)
    out.append(row)
print(json.dumps(out))
"#,
            &[dir.path()],
        ) else {
            return;
        };
        let actual: Vec<_> = paths
            .iter()
            .map(|p| {
                let p = dir.path().join(p);
                serde_json::json!([resolve(&p).ok(), realpath(&p).ok()])
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
        let non_utf8 = PathBuf::from(OsString::from_vec(b"./a\xff".to_vec()));
        assert!(resolve(&non_utf8)
            .unwrap()
            .as_os_str()
            .as_bytes()
            .ends_with(b"/a\xff"));
    }
}

/// Strict native realpath: every component must exist. Unlike `resolve`, this
/// is the fs.realpath/Path.resolve(strict=True) contract.
pub fn realpath(path: &Path) -> Result<PathBuf, PathError> {
    fs::canonicalize(path).map_err(|source| PathError::Io {
        path: path.into(),
        source,
    })
}

/// A UTF-8 boundary for consumers whose record schema requires a Rust string.
/// Use `path_sort_key` for native Python string ordering, never lossy decoding.
pub fn path_text(path: &Path) -> Result<String, PathError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| PathError::Encoding { path: path.into() })
}

/// Python os.fsdecode ordering as code points (surrogateescape on Unix).
/// No filename bytes are replaced and astral characters sort as single points.
pub fn path_sort_key(name: &std::ffi::OsStr) -> Vec<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut result = Vec::new();
        for chunk in name.as_bytes().utf8_chunks() {
            result.extend(chunk.valid().chars().map(u32::from));
            result.extend(chunk.invalid().iter().map(|b| 0xdc00 + u32::from(*b)));
        }
        result
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        char::decode_utf16(name.encode_wide())
            .map(|c| {
                c.map(u32::from)
                    .unwrap_or_else(|e| u32::from(e.unpaired_surrogate()))
            })
            .collect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        name.to_string_lossy().chars().map(u32::from).collect()
    }
}

/// Pathlib's parent spelling: the parent of a simple relative name is `.`.
pub fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}
