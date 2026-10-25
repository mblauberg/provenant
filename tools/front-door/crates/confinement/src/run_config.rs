//! `[domains.run]`: host-owned extra write paths for worktree-write lanes.
use crate::permissions::{PathGrant, PathScope, is_credential_root};
use provenant_config::{DomainConfig, DomainSection, SectionError};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

const MAX_WRITER_WRITES: usize = 16;

/// Shared run state kept outside the worktree, such as a lock directory a
/// repository script locks with `/usr/bin/lockf`, or a shared cargo target.
/// Each directory is read-write for writer lanes only; read-only lanes ignore
/// it. Plan compilation still refuses any overlap with protected roots.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    #[serde(default)]
    pub writer_writes: Vec<PathBuf>,
}
impl RunConfig {
    pub fn writer_write_roots(&self) -> impl Iterator<Item = &Path> {
        self.writer_writes.iter().map(PathBuf::as_path)
    }
}
fn valid_writer_write(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|value| {
            value.len() <= 4096
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
        })
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        && PathGrant {
            path: path.to_owned(),
            scope: PathScope::Subtree,
        }
        .validate()
        .is_ok()
        && !is_credential_root(path)
}
impl DomainConfig for RunConfig {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors = Vec::new();
        if self.writer_writes.len() > MAX_WRITER_WRITES {
            errors.push(SectionError {
                field: "writer_writes".into(),
                code: "range",
            });
        }
        for (index, path) in self.writer_writes.iter().enumerate() {
            if !valid_writer_write(path) {
                errors.push(SectionError {
                    field: format!("writer_writes.{index}"),
                    code: "path",
                });
            }
        }
        errors
    }
}
pub const SECTION: DomainSection =
    DomainSection::new::<RunConfig>("run", |_| "writer_writes = []\n");

#[cfg(test)]
mod tests {
    use super::*;
    fn fields(paths: &[&str]) -> Vec<String> {
        RunConfig {
            writer_writes: paths.iter().map(PathBuf::from).collect(),
        }
        .validate()
        .into_iter()
        .map(|error| error.field)
        .collect()
    }
    #[test]
    fn accepts_absolute_directories() {
        assert!(
            fields(&[
                "/Users/a/p/.agent-run/locks",
                "/Volumes/x/.build/cargo-wave"
            ])
            .is_empty()
        );
        assert!(fields(&[]).is_empty());
    }
    #[test]
    fn refuses_relative_unsafe_and_credential_paths() {
        assert_eq!(fields(&["locks"]), ["writer_writes.0"]);
        assert_eq!(fields(&["/a/b c"]), ["writer_writes.0"]);
        assert_eq!(fields(&["/a/$HOME"]), ["writer_writes.0"]);
        assert_eq!(fields(&["/a/../b"]), ["writer_writes.0"]);
        assert_eq!(fields(&["/Users/a"]), ["writer_writes.0"]);
        assert_eq!(
            fields(&["/Users/a/.ssh/x", "/Users/a/p"]),
            ["writer_writes.0"]
        );
        assert_eq!(fields(&["/Users/a/.aws"]), ["writer_writes.0"]);
        assert_eq!(
            fields(&["/Users/a/Archives/git-preservation"]),
            ["writer_writes.0"]
        );
    }
}
