//! Opt-in launch classification from the repository's project configuration.
use crate::ConfigError;
use provenant_os::fs::Directory;
use serde::Deserialize;
use std::{
    ffi::OsStr,
    io::Read,
    path::{Component, Path, PathBuf},
};

pub const REPOSITORY_CONFIG: &str = ".provenant/config.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryPolicy {
    #[serde(default)]
    pub confidential: bool,
    /// Repository-relative files or directory prefixes, not shell patterns.
    #[serde(default)]
    pub protected_paths: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct ProjectConfig {
    #[serde(default)]
    launch_policy: RepositoryPolicy,
}

impl RepositoryPolicy {
    /// Only absence means no policy. An unreadable or malformed declaration
    /// cannot silently remove the repository's confidentiality boundary.
    pub fn read(checkout: &Path) -> Result<Option<Self>, ConfigError> {
        let root = Directory::open_anchor(checkout)?;
        let directory = match root.open_verified_directory(OsStr::new(".provenant")) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let file = match directory.open_read(OsStr::new("config.toml")) {
            Ok(file) => file,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut text = String::new();
        file.take(64 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|_| ConfigError::InvalidRepositoryPolicy)?;
        if text.len() > 64 * 1024 {
            return Err(ConfigError::InvalidRepositoryPolicy);
        }
        let config: ProjectConfig =
            toml_edit::de::from_str(&text).map_err(|_| ConfigError::InvalidRepositoryPolicy)?;
        if config.launch_policy.protected_paths.iter().any(|path| {
            path.as_os_str().is_empty()
                || path.components().any(|part| {
                    matches!(
                        part,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
        }) {
            return Err(ConfigError::InvalidRepositoryPolicy);
        }
        Ok(Some(config.launch_policy))
    }

    /// A directory input may contain a protected file, so overlap in either
    /// direction classifies it. Canonical paths also catch symlink aliases.
    pub fn protects(&self, checkout: &Path, cwd: &Path, path: &Path) -> bool {
        let input = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        let input = normalise(&input);
        self.protected_paths.iter().any(|protected| {
            let protected = normalise(&checkout.join(protected));
            let overlaps = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
            overlaps(&input, &protected)
                || input
                    .canonicalize()
                    .ok()
                    .zip(protected.canonicalize().ok())
                    .is_some_and(|(input, protected)| overlaps(&input, &protected))
        })
    }

    pub fn protects_brief(&self, checkout: &Path, cwd: &Path, brief: &str) -> bool {
        brief
            .split(|c: char| {
                c.is_whitespace()
                    || matches!(c, '`' | '\'' | '"' | '(' | ')' | '[' | ']' | ',' | ';')
            })
            .filter(|word| !word.is_empty())
            .any(|word| self.protects(checkout, cwd, Path::new(word.trim_end_matches(':'))))
    }
}

fn normalise(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_policy_classifies_inputs_and_briefs_without_implicit_policy() {
        let root = tempfile::tempdir().unwrap();
        assert!(RepositoryPolicy::read(root.path()).unwrap().is_none());
        std::fs::create_dir(root.path().join(".provenant")).unwrap();
        std::fs::write(
            root.path().join(REPOSITORY_CONFIG),
            "[launch_policy]\nprotected_paths=['private']\n",
        )
        .unwrap();
        let policy = RepositoryPolicy::read(root.path()).unwrap().unwrap();
        assert!(policy.protects(root.path(), root.path(), Path::new("private/file.md")));
        assert!(policy.protects_brief(root.path(), root.path(), "Read `private/brief.md`"));
        assert!(!policy.protects_brief(root.path(), root.path(), "Read public/brief.md"));
        std::fs::write(
            root.path().join(REPOSITORY_CONFIG),
            "[launch_policy]\nprotected_paths=['../escape']\n",
        )
        .unwrap();
        assert!(RepositoryPolicy::read(root.path()).is_err());
    }
}
