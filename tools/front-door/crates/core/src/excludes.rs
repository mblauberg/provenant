//! Repository-local ignore rules for generated directories.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExcludeError {
    #[error("expected a real common Git directory")]
    CommonDirectory,
    #[error("Git exclude path must not be a symlink")]
    Symlink,
    #[error("cannot update Git excludes: {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn write_exclude_rules(common_git_dir: &Path, rules: &[&str]) -> Result<(), ExcludeError> {
    let common_git_dir = crate::paths::lexical_path(common_git_dir);
    if !common_git_dir.is_dir() || common_git_dir.is_symlink() {
        return Err(ExcludeError::CommonDirectory);
    }
    let parent = common_git_dir.join("info");
    let exclude = parent.join("exclude");
    if parent.is_symlink() || exclude.is_symlink() {
        return Err(ExcludeError::Symlink);
    }
    let at = |path: &Path, source| ExcludeError::Io {
        path: path.into(),
        source,
    };
    fs::create_dir_all(&parent).map_err(|e| at(&parent, e))?;
    let raw = if exclude.exists() {
        fs::read(&exclude).map_err(|e| at(&exclude, e))?
    } else {
        Vec::new()
    };
    let text = String::from_utf8_lossy(&raw).replace("\r\n", "\n");
    let mut existing: Vec<_> = text
        .split([
            '\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}',
        ])
        .collect();
    if existing.last() == Some(&"") {
        existing.pop();
    }
    // Compare to the original file only: repeated rules in this call are appended,
    // matching the Python contract; subsequent calls are idempotent.
    let missing: Vec<_> = rules.iter().filter(|r| !existing.contains(r)).collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut stream = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude)
        .map_err(|e| at(&exclude, e))?;
    if !raw.is_empty() && !raw.ends_with(b"\n") {
        stream.write_all(b"\n").map_err(|e| at(&exclude, e))?;
    }
    for rule in missing {
        writeln!(stream, "{rule}").map_err(|e| at(&exclude, e))?;
    }
    stream.flush().map_err(|e| at(&exclude, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anchored_rules_are_local_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path().join(".git");
        fs::create_dir(&common).unwrap();
        write_exclude_rules(&common, &["/.worktrees/", "/.agent-run/"]).unwrap();
        write_exclude_rules(&common, &["/.worktrees/", "/.agent-run/"]).unwrap();
        assert_eq!(
            fs::read_to_string(common.join("info/exclude")).unwrap(),
            "/.worktrees/\n/.agent-run/\n"
        );
        assert!(!dir.path().join(".gitignore").exists());
    }
    #[test]
    fn preserves_existing_bytes_and_adds_missing_newline() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("info")).unwrap();
        let exclude = dir.path().join("info/exclude");
        fs::write(&exclude, b"# keep\r\n/.worktrees/\r\n\xff").unwrap();
        write_exclude_rules(
            dir.path(),
            &["/.worktrees/", "/.agent-run/", "/.agent-run/"],
        )
        .unwrap();
        assert_eq!(
            fs::read(&exclude).unwrap(),
            b"# keep\r\n/.worktrees/\r\n\xff\n/.agent-run/\n/.agent-run/\n"
        );
        write_exclude_rules(dir.path(), &["/.agent-run/"]).unwrap();
        assert!(
            fs::read(&exclude)
                .unwrap()
                .ends_with(b"/.agent-run/\n/.agent-run/\n")
        );
    }
    #[cfg(unix)]
    #[test]
    fn refuses_linked_common_info_and_exclude() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path().join("common");
        fs::create_dir(&common).unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let alias = dir.path().join("alias");
        symlink(&common, &alias).unwrap();
        assert!(matches!(
            write_exclude_rules(&alias, &["/x"]),
            Err(ExcludeError::CommonDirectory)
        ));
        symlink(&outside, common.join("info")).unwrap();
        assert!(matches!(
            write_exclude_rules(&common, &["/x"]),
            Err(ExcludeError::Symlink)
        ));
        fs::remove_file(common.join("info")).unwrap();
        fs::create_dir(common.join("info")).unwrap();
        symlink(outside.join("missing"), common.join("info/exclude")).unwrap();
        assert!(matches!(
            write_exclude_rules(&common, &["/x"]),
            Err(ExcludeError::Symlink)
        ));
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }
}

#[cfg(test)]
mod differential {
    use super::*;
    #[test]
    fn python_exclude_effects_and_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let Some(expected) = crate::test_support::python(
            "skills/_shared/excludes.py",
            r#"
p = pathlib.Path(sys.argv[3]); p.mkdir(); (p/'info').mkdir()
(p/'info/exclude').write_bytes(b'# keep\r\n/old\r\n\xff')
m.write_exclude_rules(p,'/old','/new','/new')
m.write_exclude_rules(p,'/new')
print(json.dumps(list((p/'info/exclude').read_bytes())))
"#,
            &[&dir.path().join("python")],
        ) else {
            return;
        };
        let rust = dir.path().join("rust");
        fs::create_dir_all(rust.join("info")).unwrap();
        fs::write(rust.join("info/exclude"), b"# keep\r\n/old\r\n\xff").unwrap();
        write_exclude_rules(&rust, &["/old", "/new", "/new"]).unwrap();
        write_exclude_rules(&rust, &["/new"]).unwrap();
        assert_eq!(
            serde_json::json!(fs::read(rust.join("info/exclude")).unwrap()),
            expected
        );
    }
}
