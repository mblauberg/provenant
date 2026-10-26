//! Explicit project write exceptions for otherwise read-only attempts.
use crate::{
    ReadOnlyRepo,
    permissions::{PathGrant, PathScope, PermissionError},
    plan::valid_read_only_writable,
};
use provenant_core::RelativePath;
use provenant_supervisor::fs::SafeDir;
use serde::Deserialize;
use std::{ffi::OsStr, io::Read, path::Component};

/// Stored in the host's project state, never loaded from lane-controlled config.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadOnlyPolicy {
    #[serde(default, rename = "readOnlyWritable")]
    writable: Vec<String>,
}

impl ReadOnlyPolicy {
    pub fn read(project: &dyn SafeDir) -> Result<Self, PermissionError> {
        let file = match project.open_read(OsStr::new("read-only.json")) {
            Ok(file) => file,
            Err(e) if e.is_not_found() => return Ok(Self::default()),
            Err(e) => return Err(PermissionError::Filesystem(e)),
        };
        if file.metadata().map_err(PermissionError::Filesystem)?.links != 1 {
            return Err(PermissionError::InvalidReadOnlyPolicy);
        }
        let mut bytes = Vec::new();
        file.take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(PermissionError::HeadRead)?;
        if bytes.len() > 16 * 1024 {
            return Err(PermissionError::InvalidReadOnlyPolicy);
        }
        serde_json::from_slice(&bytes).map_err(|_| PermissionError::InvalidReadOnlyPolicy)
    }

    pub(crate) fn resolve(
        &self,
        repo: &ReadOnlyRepo,
        binding: &provenant_config::ProjectBinding,
    ) -> Result<ReadOnlyGrants, PermissionError> {
        repo.0.revalidate()?;
        if repo.0.common_git_dir() != binding.common_dir() {
            return Err(PermissionError::InvalidReadOnlyPolicy);
        }
        if !valid_read_only_writable(&self.writable) {
            return Err(PermissionError::InvalidReadOnlyPolicy);
        }
        let mut writes = Vec::new();
        for relative in &self.writable {
            let path = RelativePath::parse(relative)
                .map_err(|_| PermissionError::InvalidReadOnlyPolicy)?;
            if path
                .as_path()
                .components()
                .any(|c| matches!(c, Component::Normal(name) if reserved(name)))
            {
                return Err(PermissionError::InvalidReadOnlyPolicy);
            }
            verify_directory_path(repo.0.root_dir(), &path)?;
            writes.push(PathGrant {
                path: repo.0.root_path().join(path.as_path()),
                scope: PathScope::Subtree,
            });
        }
        Ok(ReadOnlyGrants {
            #[cfg(any(target_os = "macos", test))]
            root: repo.0.root_path().to_owned(),
            // The host binding is authority; the lane's Git marker is not.
            // Its subtree includes each linked worktree's private directory.
            git_reads: [PathGrant {
                path: binding.common_dir().to_owned(),
                scope: PathScope::Subtree,
            }],
            writes,
            relative: self.writable.clone(),
        })
    }
}

/// Host-resolved roots. There is deliberately no deserialisation or public
/// constructor that can turn a lane's Git marker into authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadOnlyGrants {
    #[cfg(any(target_os = "macos", test))]
    pub(crate) root: std::path::PathBuf,
    pub(crate) git_reads: [PathGrant; 1],
    pub(crate) writes: Vec<PathGrant>,
    relative: Vec<String>,
}
impl ReadOnlyGrants {
    #[cfg(test)]
    pub(crate) fn for_test(root: std::path::PathBuf, writes: Vec<PathGrant>) -> Self {
        Self {
            git_reads: [PathGrant {
                path: root.join(".git"),
                scope: PathScope::Subtree,
            }],
            root,
            writes,
            relative: Vec::new(),
        }
    }

    pub fn writable_paths(&self) -> &[String] {
        &self.relative
    }
    pub fn git_read_paths(&self) -> impl Iterator<Item = &std::path::Path> {
        self.git_reads.iter().map(|grant| grant.path.as_path())
    }
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn revalidate(
        &self,
        fs: &dyn provenant_supervisor::fs::SafeFs,
    ) -> Result<(), PermissionError> {
        let root = fs
            .open_anchor(&self.root)
            .map_err(PermissionError::Filesystem)?;
        for path in &self.relative {
            let relative =
                RelativePath::parse(path).map_err(|_| PermissionError::InvalidReadOnlyPolicy)?;
            verify_directory_path(root.as_ref(), &relative)?;
        }
        Ok(())
    }
}

fn verify_directory_path(root: &dyn SafeDir, path: &RelativePath) -> Result<(), PermissionError> {
    let mut directory: Option<Box<dyn SafeDir>> = None;
    for component in path.as_path().components() {
        if let Component::Normal(name) = component {
            let parent = directory.as_deref().unwrap_or(root);
            directory = Some(
                parent
                    .open_verified_directory(name)
                    .map_err(PermissionError::Filesystem)?,
            );
        }
    }
    Ok(())
}

// .config covers XDG provider configuration; .github includes Copilot.
pub(crate) const RESERVED: &[&str] = &[
    ".git",
    ".agents",
    ".codex",
    ".claude",
    ".cursor",
    ".gemini",
    ".opencode",
    ".kiro",
    ".copilot",
    ".github",
    ".config",
    ".aider",
    ".continue",
    ".vscode",
    ".windsurf",
    ".claude.json",
    ".mcp.json",
    "opencode.json",
    "opencode.jsonc",
    "AGENTS.md",
    "CLAUDE.md",
    "GEMINI.md",
];
fn reserved(name: &OsStr) -> bool {
    RESERVED
        .iter()
        .any(|n| name.as_encoded_bytes().eq_ignore_ascii_case(n.as_bytes()))
}
#[cfg(target_os = "macos")]
pub(crate) fn exclusion_regex() -> String {
    let names: Vec<String> = RESERVED
        .iter()
        .map(|name| {
            name.chars()
                .map(|c| {
                    if c == '.' {
                        "[.]".to_owned()
                    } else {
                        format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase())
                    }
                })
                .collect()
        })
        .collect();
    format!("/({})(/|$)", names.join("|"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_supervisor::fs::{LocalFs, SafeFs};
    #[test]
    fn policy_defaults_and_refuses_escape_control_paths_and_symlinks()
    -> Result<(), Box<dyn std::error::Error>> {
        let f = crate::test_support::Fixture::new()?;
        let binding = f.binding()?;
        let repo = f.runtime.open_read_repo(
            &f.executor,
            f.repo.root_path(),
            f.budget()?,
            &provenant_core::CancelToken::new(),
        )?;
        let root = repo.0.root_path();
        std::fs::create_dir_all(root.join(".agent-run/locks"))?;
        std::os::unix::fs::symlink(root.join(".agent-run"), root.join("alias"))?;
        let policy = |paths: &[&str]| -> ReadOnlyPolicy {
            serde_json::from_value(serde_json::json!({"readOnlyWritable": paths})).unwrap()
        };
        assert!(
            ReadOnlyPolicy::default()
                .resolve(&repo, &binding)?
                .writes
                .is_empty()
        );
        let grants = policy(&[".agent-run/locks"]).resolve(&repo, &binding)?;
        assert_eq!(grants.writes[0].path, root.join(".agent-run/locks"));
        assert_eq!(
            grants.git_read_paths().collect::<Vec<_>>(),
            [binding.common_dir()]
        );
        grants.revalidate(&LocalFs)?;
        for path in [
            "/tmp/x",
            "../escape",
            "a/../b",
            ".",
            ".git/objects",
            "sub/.agents/cache",
            ".CODEX/tmp",
            ".claude",
            ".cursor",
            ".gemini",
            ".config/opencode",
            "cache/opencode.json",
            "cache/.mcp.json",
            "cache/AGENTS.md",
            "alias/locks",
        ] {
            if RelativePath::parse(path).is_ok() && path != "alias/locks" {
                std::fs::create_dir_all(root.join(path))?;
            }
            assert!(policy(&[path]).resolve(&repo, &binding).is_err(), "{path}");
        }
        let other = crate::test_support::Fixture::new()?;
        assert!(
            ReadOnlyPolicy::default()
                .resolve(&repo, &other.binding()?)
                .is_err()
        );
        let project = tempfile::tempdir()?;
        let dir = LocalFs.open_anchor(project.path())?;
        assert!(ReadOnlyPolicy::read(dir.as_ref())?.writable.is_empty());
        std::fs::write(project.path().join("read-only.json"), b"{broken")?;
        assert!(ReadOnlyPolicy::read(dir.as_ref()).is_err());
        std::fs::remove_file(project.path().join("read-only.json"))?;
        std::os::unix::fs::symlink(
            root.join("tracked.txt"),
            project.path().join("read-only.json"),
        )?;
        assert!(ReadOnlyPolicy::read(dir.as_ref()).is_err());
        Ok(())
    }
}
