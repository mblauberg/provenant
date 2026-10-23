//! Path inspection only. This is not a descriptor-bound custody guarantee.
//!
//! Descriptor-bound opens, reads, directory creation, publication and unlinking
//! remain blocked on the supervisor's safe OS API (PORTING.md R7).

pub use crate::pending::OwnedFileError;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// Inspect a run-relative regular single-link path, matching Python's helper.
/// Callers must not use this check followed by a pathname open as safe custody.
#[cfg(unix)]
pub fn contained_regular_path(
    root: &Path,
    value: &Path,
    label: &str,
) -> Result<(PathBuf, PathBuf), OwnedFileError> {
    let mut relative = PathBuf::new();
    for part in value.components() {
        match part {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            _ => {
                return Err(OwnedFileError::Refused(format!(
                    "{label} must be a run-relative path"
                )));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(OwnedFileError::Refused(format!(
            "{label} must be a run-relative path"
        )));
    }
    let root = fs::canonicalize(root).map_err(|source| OwnedFileError::Io {
        message: format!("{label} run root is unavailable"),
        source,
    })?;
    let metadata = fs::symlink_metadata(&root).map_err(|source| OwnedFileError::Io {
        message: format!("{label} run root is unavailable"),
        source,
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(OwnedFileError::Refused(format!(
            "{label} run root is not a real directory"
        )));
    }
    let raw = relative.display();
    let mut target = root.clone();
    for part in relative.components() {
        target.push(part);
        let metadata = fs::symlink_metadata(&target).map_err(|source| OwnedFileError::Io {
            message: format!("{label} is unavailable: {raw}"),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(OwnedFileError::Refused(format!(
                "{label} must not use a symlink: {raw}"
            )));
        }
    }
    let metadata = fs::symlink_metadata(&target).map_err(|source| OwnedFileError::Io {
        message: format!("{label} is outside the run: {raw}"),
        source,
    })?;
    let resolved = fs::canonicalize(&target).map_err(|source| OwnedFileError::Io {
        message: format!("{label} is outside the run: {raw}"),
        source,
    })?;
    if !resolved.starts_with(&root) {
        return Err(OwnedFileError::Refused(format!(
            "{label} is outside the run: {raw}"
        )));
    }
    let message = format!("{label} must be a regular single-link file: {raw}");
    if !metadata.is_file() {
        return Err(OwnedFileError::Refused(message));
    }
    if metadata.nlink() != 1 {
        return Err(OwnedFileError::MultipleLinks(message));
    }
    Ok((relative, target))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn paths_reject_traversal_links_directories_and_missing_files() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested/file"), b"owned").unwrap();
        let (relative, target) =
            contained_regular_path(root.path(), Path::new("./nested//file"), "evidence").unwrap();
        assert_eq!(relative, Path::new("nested/file"));
        assert_eq!(fs::read(target).unwrap(), b"owned");
        for value in [
            "",
            ".",
            "..",
            "a/../nested/file",
            "/absolute",
            "missing",
            "nested",
        ] {
            assert!(
                contained_regular_path(root.path(), Path::new(value), "evidence").is_err(),
                "{value}"
            );
        }
        symlink("nested", root.path().join("link")).unwrap();
        assert_eq!(
            contained_regular_path(root.path(), Path::new("link/file"), "evidence")
                .unwrap_err()
                .to_string(),
            "evidence must not use a symlink: link/file"
        );
        fs::hard_link(root.path().join("nested/file"), root.path().join("hard")).unwrap();
        assert!(matches!(
            contained_regular_path(root.path(), Path::new("hard"), "evidence"),
            Err(OwnedFileError::MultipleLinks(_))
        ));
    }

    #[test]
    fn root_symlinks_resolve_and_non_utf8_names_stay_os_paths() {
        use std::os::unix::ffi::OsStrExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let link = temp.path().join("alias");
        symlink("root", &link).unwrap();
        fs::write(root.join("plain"), b"ok").unwrap();
        assert_eq!(
            contained_regular_path(&link, Path::new("plain"), "file")
                .unwrap()
                .0,
            Path::new("plain")
        );
        let name = Path::new(std::ffi::OsStr::from_bytes(b"evidence-\xff"));
        match fs::write(root.join(name), b"ok") {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput
                ) || error.raw_os_error() == Some(92) =>
            {
                eprintln!("skip: non-UTF-8 filesystem fixture: {error}");
                return;
            }
            Err(error) => panic!("cannot create non-UTF-8 fixture: {error}"),
        }
        assert_eq!(contained_regular_path(&link, name, "file").unwrap().0, name);
    }

    #[test]
    fn path_differential_includes_refusals_and_link_identity() {
        use serde_json::json;
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("nested")).unwrap();
        fs::write(temp.path().join("nested/file"), "owned").unwrap();
        fs::write(temp.path().join("single"), "owned").unwrap();
        symlink("nested", temp.path().join("link")).unwrap();
        fs::hard_link(temp.path().join("nested/file"), temp.path().join("hard")).unwrap();
        let mut cases = Vec::new();
        let mut actual = Vec::new();
        let canonical_root = fs::canonicalize(temp.path()).unwrap();
        for root in [
            canonical_root.clone(),
            canonical_root.join("absent"),
            canonical_root.join("single"),
        ] {
            for path in [
                "",
                ".",
                "../escape",
                "/absolute",
                "single",
                "./single",
                "nested/file",
                "nested",
                "link/file",
                "hard",
                "missing",
                "a/../single",
            ] {
                cases.push(json!({"root":root,"path":path}));
                actual.push(match contained_regular_path(&root, Path::new(path), "file") {
                    Ok((relative, target)) => json!({"relative":relative,"target":target}),
                    Err(error) => json!({"error":error.to_string(),"hardlink":matches!(error, OwnedFileError::MultipleLinks(_))}),
                });
            }
        }
        if let Some(expected) = crate::test_support::oracle("custody", &json!(cases)) {
            assert_eq!(json!(actual), expected);
        }
    }
}
