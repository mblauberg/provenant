//! Fixture roots, product roots and executable-owner policy.
use crate::field::Field;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RunRootError {
    #[error("AGENT_FABRIC_PRODUCT_ROOT must be an absolute path")]
    ProductRootRelative,
    #[error("fixture run refused: {0} is not under an acceptable temp root ({1}) or a .agent-run/scratch tree; fixture attempts must not write into a project .agent-run (issue #1002)")]
    FixtureRefused(String, String),
    #[error("execution owner is unavailable: {0}")]
    OwnerUnavailable(PathBuf),
    #[error("execution owner must be a regular local file: {0}")]
    OwnerNotLocalFile(PathBuf),
    #[error(transparent)]
    Cwd(#[from] std::io::Error),
}

pub fn canonical(path: &Path) -> Result<PathBuf, RunRootError> {
    fs::canonicalize(path)
        .or_else(|_| absolute_normal(path))
        .map_err(RunRootError::Cwd)
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
            return Err(RunRootError::ProductRootRelative)
        }
        Some(value) => PathBuf::from(value),
        None => default_root.to_path_buf(),
    };
    canonical(&path)
}

/// X_OK is owned by supervisor; symlinks remain refused even when executable.
pub fn executable_owner(root: &Path, relative: &Path) -> Result<PathBuf, RunRootError> {
    // Node path.join concatenates an absolute second operand rather than
    // discarding the root as Path::join does.
    let mut spelling = root.as_os_str().to_os_string();
    if !spelling.is_empty() {
        spelling.push("/");
    }
    spelling.push(relative.as_os_str());
    let joined = PathBuf::from(spelling);
    let mut path = if joined.is_absolute() {
        absolute_normal(&joined)?
    } else {
        let mut result = PathBuf::new();
        for part in joined.components() {
            match part {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir
                    if result.file_name().is_some_and(|name| name != "..") =>
                {
                    result.pop();
                }
                part => result.push(part.as_os_str()),
            }
        }
        if result.as_os_str().is_empty() {
            result.push(".");
        }
        result
    };
    // Node normalises components but retains a final separator. It changes
    // both filesystem lookup (a regular file becomes unavailable) and errors.
    let last = if relative.as_os_str().is_empty() { root } else { relative };
    if last.as_os_str().as_encoded_bytes().ends_with(b"/")
        && !path.as_os_str().as_encoded_bytes().ends_with(b"/")
    {
        let mut spelling = path.into_os_string();
        spelling.push("/");
        path = PathBuf::from(spelling);
    }
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| RunRootError::OwnerUnavailable(path.clone()))?;
    if !provenant_supervisor::sys::is_executable(&path) {
        return Err(RunRootError::OwnerUnavailable(path));
    }
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(RunRootError::OwnerNotLocalFile(path));
    }
    Ok(path)
}

fn real_path(path: &Path) -> Result<PathBuf, RunRootError> {
    let absolute = absolute_normal(path).map_err(RunRootError::Cwd)?;
    let mut candidate = absolute.clone();
    let mut missing = Vec::new();
    loop {
        if let Ok(real) = fs::canonicalize(&candidate) {
            let mut result = real;
            for component in missing.iter().rev() {
                result.push(component);
            }
            return absolute_normal(&result).map_err(RunRootError::Cwd);
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
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub model: Field<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub cli_version: Field<serde_json::Value>,
}

pub fn assert_fixture_run_root(
    agent_run: &Path,
    routes: &[FixtureRoute],
    temp: &Path,
    fabric_test_tmpdir: Option<&Path>,
) -> Result<(), RunRootError> {
    if !routes.iter().any(|route| {
        matches!(&route.model, Field::Value(value) if value.as_str() == Some("fixture"))
            || matches!(&route.cli_version, Field::Value(value) if value.as_str() == Some("fixture"))
    }) {
        return Ok(());
    }
    let root = real_path(agent_run)?;
    let mut temp_roots = vec![temp.to_path_buf()];
    if let Some(path) = fabric_test_tmpdir.filter(|path| !path.as_os_str().is_empty()) {
        temp_roots.push(path.to_path_buf());
    }
    temp_roots.push(PathBuf::from("/tmp"));
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
        .any(|path| path != Path::new("/") && root.starts_with(path) && root != *path);
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
        assert!(matches!(
            product_root(Some("relative"), Path::new("/default")),
            Err(RunRootError::ProductRootRelative)
        ));
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
            model: Field::Value(serde_json::Value::String("fixture".into())),
            cli_version: Field::Missing,
        }];
        let under_temp = assert_fixture_run_root(
            &temp.path().join("workspace/.agent-run"),
            &route,
            temp.path(),
            None,
        );
        assert!(under_temp.is_ok(), "{under_temp:?}");
        assert!(assert_fixture_run_root(
            Path::new("/repo/.agent-run/scratch/run/.agent-run"),
            &route,
            temp.path(),
            None
        )
        .is_ok());
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
        let link = temp.path().join("dangling");
        symlink(temp.path().join("missing-target"), &link).unwrap();
        assert_eq!(
            real_path(&link).unwrap(),
            fs::canonicalize(temp.path()).unwrap().join("dangling")
        );
    }

    #[test]
    fn fixture_route_accepts_non_string_source_values() {
        let route: FixtureRoute = serde_json::from_value(serde_json::json!({
            "model": 17,
            "cli_version": {"name": "fixture"}
        }))
        .unwrap();
        assert_eq!(route.model, Field::Value(serde_json::json!(17)));
    }

    #[test]
    fn node_differential_covers_fixture_fences_errors_product_root_and_optional_fields() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let paths = vec![
            root.path().to_path_buf(),
            repo.clone(),
            repo.join("sub"),
            root.path().join(".agent-run/runs"),
            root.path().join(".agent-run"),
        ];
        let cases = serde_json::json!([
            ["/repo/.agent-run", [{"model":"fixture"}]],
            ["/repo/.agent-run/scratch", [{"model":"fixture"}]],
            ["/repo/.agent-run/scratch/run/.agent-run", [{"cli_version":"fixture"}]],
            ["/tmp/fr3-fixture/.agent-run", [{"model":"fixture"}]],
            ["/repo/.agent-run", [{"model":17}]],
            [root.path().join("child/.agent-run"), [{"model":"fixture"}]]
        ]);
        let script = r#"import * as m from './src/run-root.ts';
const [paths,cases,temp,configured,defaultRoot] = process.argv.slice(1).map(JSON.parse);
process.env.FABRIC_TEST_TMPDIR = configured;
const capture = f => {try { return f() ?? null; } catch(e) {return e.message;}};
console.log(JSON.stringify({
 roots: paths.map(m.acceptableFixtureTempRoot),
 fixtures: cases.map(([path,routes])=>capture(()=>m.assertFixtureRunRoot(path,routes,temp))),
 products:[capture(()=>m.productRoot({AGENT_FABRIC_PRODUCT_ROOT:'relative'})),m.productRoot({AGENT_FABRIC_PRODUCT_ROOT:configured}),m.productRoot({})]
}));"#;
        let default_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let args = [
            serde_json::to_string(&paths).unwrap(),
            cases.to_string(),
            serde_json::to_string(&repo).unwrap(),
            serde_json::to_string(root.path()).unwrap(),
            serde_json::to_string(&default_root).unwrap(),
        ];
        let Some(expected) = crate::test_support::node(script, &args) else {
            return;
        };
        let fixtures: Vec<_> = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|case| {
                let routes: Vec<FixtureRoute> = serde_json::from_value(case[1].clone()).unwrap();
                assert_fixture_run_root(
                    Path::new(case[0].as_str().unwrap()),
                    &routes,
                    &repo,
                    Some(root.path()),
                )
                .err()
                .map(|error| error.to_string())
            })
            .collect();
        assert_eq!(
            serde_json::json!({
                "roots":paths.iter().map(|path| acceptable_fixture_temp_root(path).unwrap()).collect::<Vec<_>>(),
                "fixtures":fixtures,
                "products":[product_root(Some("relative"), &default_root).unwrap_err().to_string(),
                    product_root(Some(root.path().to_str().unwrap()), &default_root).unwrap().to_string_lossy(),
                    product_root(None, &default_root).unwrap().to_string_lossy()]
            }),
            expected
        );
        let absent: FixtureRoute = serde_json::from_value(serde_json::json!({})).unwrap();
        let null: FixtureRoute = serde_json::from_value(serde_json::json!({"model":null})).unwrap();
        assert_eq!(serde_json::to_value(absent).unwrap(), serde_json::json!({}));
        assert_eq!(
            serde_json::to_value(null).unwrap(),
            serde_json::json!({"model":null})
        );
    }
    #[cfg(unix)]
    #[test]
    fn executable_owner_matches_node_success_and_failure_order() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("owner"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(root.path().join("owner"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.path().join("plain"), "plain").unwrap();
        fs::set_permissions(root.path().join("plain"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir(root.path().join("directory")).unwrap();
        symlink("owner", root.path().join("link")).unwrap();
        symlink("missing", root.path().join("dangling")).unwrap();
        let paths = [
            "owner",
            "owner/",
            "directory/",
            "plain",
            "directory",
            "link",
            "dangling",
            "missing",
            "/owner",
            "./owner",
            "directory/../owner",
        ];
        let script = r#"import {executableOwner} from './src/run-root.ts';
const [root,paths]=process.argv.slice(1); console.log(JSON.stringify(JSON.parse(paths).map(path=>{
try{return {ok:executableOwner(root,path)}}catch(e){return {error:e.message}}})));"#;
        let Some(expected) = crate::test_support::node(
            script,
            &[
                root.path().to_str().unwrap().into(),
                serde_json::to_string(&paths).unwrap(),
            ],
        ) else {
            return;
        };
        let actual: Vec<_> = paths
            .iter()
            .map(
                |path| match executable_owner(root.path(), Path::new(path)) {
                    Ok(path) => serde_json::json!({"ok":path}),
                    Err(error) => serde_json::json!({"error":error.to_string()}),
                },
            )
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}
