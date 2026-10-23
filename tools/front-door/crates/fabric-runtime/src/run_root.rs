//! Fixture-root and product-root policy. Executable owner probing remains at the OS edge.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RunRootError {
    #[error("AGENT_FABRIC_PRODUCT_ROOT must be an absolute path")]
    ProductRootRelative,
    #[error(
        "fixture run refused: {0} is not under an acceptable temp root ({1}) or a .agent-run/scratch tree; fixture attempts must not write into a project .agent-run (issue #1002)"
    )]
    FixtureRefused(String, String),
    #[error("could not resolve path: {0}")]
    Resolve(String),
}

pub fn canonical(path: &Path) -> Result<PathBuf, RunRootError> {
    fs::canonicalize(path)
        .or_else(|_| absolute_normal(path))
        .map_err(|_| RunRootError::Resolve(path.display().to_string()))
}

fn absolute_normal(path: &Path) -> std::io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                result.pop();
            }
            component => result.push(component.as_os_str()),
        }
    }
    Ok(result)
}

pub fn product_root(
    configured: Option<&str>,
    default_root: &Path,
) -> Result<PathBuf, RunRootError> {
    let path = match configured {
        Some(value) if !Path::new(value).is_absolute() => {
            return Err(RunRootError::ProductRootRelative);
        }
        Some(value) => PathBuf::from(value),
        None => default_root.to_path_buf(),
    };
    canonical(&path)
}

fn real_path(path: &Path) -> Result<PathBuf, RunRootError> {
    let absolute =
        absolute_normal(path).map_err(|_| RunRootError::Resolve(path.display().to_string()))?;
    let mut candidate = absolute.clone();
    let mut missing = Vec::new();
    loop {
        if let Ok(real) = fs::canonicalize(&candidate) {
            let mut result = real;
            for component in missing.iter().rev() {
                result.push(component);
            }
            return absolute_normal(&result)
                .map_err(|_| RunRootError::Resolve(path.display().to_string()));
        }
        let Some(parent) = candidate.parent() else {
            return Ok(absolute);
        };
        if parent == candidate {
            return Ok(absolute);
        }
        if let Some(name) = candidate.file_name() {
            missing.push(name.to_os_string());
        }
        candidate = parent.to_path_buf();
    }
}

/// Fixture temp roots must stay outside Git work trees and run-root directories.
pub fn acceptable_fixture_temp_root(root: &Path) -> Result<bool, RunRootError> {
    let real = real_path(root)?;
    let basename = real.file_name().unwrap_or_default();
    if basename == ".agent-run"
        || (basename == "runs"
            && real
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == ".agent-run"))
    {
        return Ok(false);
    }
    let mut directory = real.as_path();
    loop {
        if fs::metadata(directory.join(".git")).is_ok() {
            return Ok(false);
        }
        let Some(parent) = directory.parent() else {
            return Ok(true);
        };
        if parent == directory {
            return Ok(true);
        }
        directory = parent;
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FixtureRoute {
    #[serde(default)]
    pub model: Option<serde_json::Value>,
    #[serde(default)]
    pub cli_version: Option<serde_json::Value>,
}

pub fn assert_fixture_run_root(
    agent_run: &Path,
    routes: &[FixtureRoute],
    temp: &Path,
    fabric_test_tmpdir: Option<&Path>,
) -> Result<(), RunRootError> {
    if !routes.iter().any(|route| {
        route.model.as_ref().and_then(serde_json::Value::as_str) == Some("fixture")
            || route
                .cli_version
                .as_ref()
                .and_then(serde_json::Value::as_str)
                == Some("fixture")
    }) {
        return Ok(());
    }
    let root = real_path(agent_run)?;
    let mut temp_roots = vec![temp.to_path_buf(), PathBuf::from("/tmp")];
    if let Some(path) = fabric_test_tmpdir {
        temp_roots.push(path.to_path_buf());
    }
    let mut real_roots = Vec::new();
    for path in temp_roots {
        let path = real_path(&path)?;
        if !real_roots.contains(&path) {
            real_roots.push(path);
        }
    }
    let mut acceptable = Vec::new();
    for path in real_roots {
        if acceptable_fixture_temp_root(&path)? {
            acceptable.push(path);
        }
    }
    let under_temp = acceptable
        .iter()
        .any(|path| root.starts_with(path) && root != *path);
    if under_temp || under_scratch(&root) {
        return Ok(());
    }
    let shown = acceptable
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(" or ");
    Err(RunRootError::FixtureRefused(
        root.display().to_string(),
        shown,
    ))
}

fn under_scratch(root: &Path) -> bool {
    let parts = root
        .components()
        .map(|part| part.as_os_str())
        .collect::<Vec<_>>();
    parts
        .windows(3)
        .any(|parts| parts[0] == ".agent-run" && parts[1] == "scratch")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn product_root_requires_absolute_configuration_and_canonicalises_paths() {
        assert_eq!(
            product_root(Some("relative"), Path::new("/default")),
            Err(RunRootError::ProductRootRelative)
        );
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            product_root(None, root.path()).unwrap(),
            fs::canonicalize(root.path()).unwrap()
        );
    }

    #[test]
    fn fixture_temp_roots_refuse_git_worktrees_and_run_directories() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::create_dir_all(root.path().join("tmp")).unwrap();
        assert!(!acceptable_fixture_temp_root(&root.path().join("tmp")).unwrap());
        assert!(!acceptable_fixture_temp_root(Path::new("/x/.agent-run/runs")).unwrap());
    }

    #[test]
    fn fixtures_are_bounded_to_temp_or_scratch_but_real_routes_are_unrestricted() {
        let temp = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
        let route = [FixtureRoute {
            model: Some(serde_json::Value::String("fixture".into())),
            cli_version: None,
        }];
        let under_temp = assert_fixture_run_root(
            &temp.path().join("workspace/.agent-run"),
            &route,
            temp.path(),
            None,
        );
        assert!(under_temp.is_ok(), "{under_temp:?}");
        assert!(
            assert_fixture_run_root(
                Path::new("/repo/.agent-run/scratch/run/.agent-run"),
                &route,
                temp.path(),
                None
            )
            .is_ok()
        );
        assert!(matches!(
            assert_fixture_run_root(Path::new("/repo/.agent-run"), &route, temp.path(), None),
            Err(RunRootError::FixtureRefused(..))
        ));
        assert!(
            assert_fixture_run_root(Path::new("/repo/.agent-run"), &[], temp.path(), None).is_ok()
        );
    }

    #[cfg(unix)]
    #[test]
    fn fixture_temp_root_follows_symlinks_before_applying_the_git_fence() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir(repo.path().join(".git")).unwrap();
        let link = temp.path().join("link");
        symlink(repo.path(), &link).unwrap();
        assert!(!acceptable_fixture_temp_root(&link).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn real_path_keeps_a_dangling_symlink_lexical_path() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().canonicalize().unwrap();
        let link = dir.join("dangling");
        symlink(dir.join("missing-target"), &link).unwrap();
        assert_eq!(real_path(&link).unwrap(), link);
    }

    #[test]
    fn fixture_route_accepts_non_string_source_values() {
        let route: FixtureRoute = serde_json::from_value(serde_json::json!({
            "model": 17,
            "cli_version": {"name": "fixture"}
        }))
        .unwrap();
        assert_eq!(route.model, Some(serde_json::json!(17)));
    }
}
