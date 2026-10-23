//! Workspace containment and authority scope for receipt paths.
use crate::paths::{PathError, resolve};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub enum ScopeKind {
    Artifact,
    Source,
}
impl ScopeKind {
    fn names(self) -> (&'static str, &'static str) {
        match self {
            Self::Artifact => ("allowed_artifact_paths", "artifact path"),
            Self::Source => ("allowed_source_paths", "evidence source"),
        }
    }
}
#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("{field} must be safe and workspace-relative")]
    Unsafe { field: String },
    #[error("{field} escapes the workspace")]
    Escape { field: String },
    #[error("authority.{key} must be a non-empty list")]
    EmptyScope { key: &'static str },
    #[error("authority.{key} contains an invalid path")]
    InvalidScope { key: &'static str },
    #[error("{noun} leaves authority.{key}")]
    OutsideScope {
        key: &'static str,
        noun: &'static str,
    },
    #[error("evidence source does not exist{suffix}: {source_path}")]
    MissingSource {
        source_path: String,
        suffix: &'static str,
    },
    #[error(transparent)]
    Path(#[from] PathError),
}

pub fn safe_workspace_path(
    workspace: &Path,
    value: &str,
    field: &str,
) -> Result<(PathBuf, String), WorkspaceError> {
    let path = crate::paths::lexical_path(Path::new(value));
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|c| c == Component::ParentDir)
    {
        return Err(WorkspaceError::Unsafe {
            field: field.into(),
        });
    }
    let target = resolve(&workspace.join(&path))?;
    let anchor = workspace.as_os_str().as_encoded_bytes();
    let double_root = anchor.starts_with(b"//") && !anchor.starts_with(b"///");
    if !target.starts_with(workspace) || double_root {
        return Err(WorkspaceError::Escape {
            field: field.into(),
        });
    }
    let normal: PathBuf = path
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect();
    let relative = if normal.as_os_str().is_empty() {
        ".".into()
    } else {
        normal.to_string_lossy().into_owned()
    };
    Ok((target, relative))
}

/// The run record is owned by the workflow crate; this boundary reads only its
/// authority value and preserves validation of malformed JSON shapes.
pub fn ensure_within_scope(
    run: &Value,
    workspace: &Path,
    target: &Path,
    kind: ScopeKind,
) -> Result<(), WorkspaceError> {
    let (key, noun) = kind.names();
    let target = resolve(target)?;
    let scopes = run
        .get("authority")
        .and_then(|a| a.get(key))
        .and_then(Value::as_array)
        .filter(|s| !s.is_empty())
        .ok_or(WorkspaceError::EmptyScope { key })?;
    let mut roots = Vec::new();
    for scope in scopes {
        let scope = scope.as_str().ok_or(WorkspaceError::InvalidScope { key })?;
        roots.push(safe_workspace_path(workspace, scope, &format!("authority.{key}"))?.0);
    }
    if !roots.iter().any(|r| target.starts_with(r)) {
        return Err(WorkspaceError::OutsideScope { key, noun });
    }
    Ok(())
}
pub fn check_evidence_sources(
    run: &Value,
    workspace: &Path,
    sources: &[String],
    after_command: bool,
) -> Result<(), WorkspaceError> {
    for source in sources {
        let (target, _) = safe_workspace_path(workspace, source, "evidence source")?;
        ensure_within_scope(run, workspace, &target, ScopeKind::Source)?;
        if !target.exists() {
            return Err(WorkspaceError::MissingSource {
                source_path: source.into(),
                suffix: if after_command {
                    " after command execution"
                } else {
                    ""
                },
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn authority() -> Value {
        serde_json::json!({"authority":{"allowed_artifact_paths":["allowed"],"allowed_source_paths":["allowed"]}})
    }
    #[test]
    fn safe_relative_paths_and_scope_policy() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        for path in ["", "/abs", "../outside", "a/../b"] {
            assert_eq!(
                safe_workspace_path(&root, path, "artifact")
                    .unwrap_err()
                    .to_string(),
                "artifact must be safe and workspace-relative"
            );
        }
        for (path, normal) in [
            ("./a//b/", "a/b"),
            (".", "."),
            ("./", "."),
            ("x/./y", "x/y"),
        ] {
            assert_eq!(safe_workspace_path(&root, path, "path").unwrap().1, normal);
        }
        for kind in [ScopeKind::Artifact, ScopeKind::Source] {
            ensure_within_scope(&authority(), &root, &root.join("allowed/new"), kind).unwrap();
            ensure_within_scope(&authority(), &root, &root.join("allowed"), kind).unwrap();
            assert!(matches!(
                ensure_within_scope(&authority(), &root, &root.join("allowed-extra"), kind),
                Err(WorkspaceError::OutsideScope { .. })
            ));
        }
    }
    #[test]
    fn malformed_authority_and_all_scope_entries_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        for run in [
            Value::Null,
            serde_json::json!({"authority":[]}),
            serde_json::json!({"authority":{"allowed_source_paths":[]}}),
            serde_json::json!({"authority":{"allowed_source_paths":"."}}),
        ] {
            assert!(matches!(
                ensure_within_scope(&run, &root, &root, ScopeKind::Source),
                Err(WorkspaceError::EmptyScope { .. })
            ));
        }
        for run in [
            serde_json::json!({"authority":{"allowed_source_paths":[".",42]}}),
            serde_json::json!({"authority":{"allowed_source_paths":[".","../escape"]}}),
        ] {
            assert!(ensure_within_scope(&run, &root, &root, ScopeKind::Source).is_err());
        }
    }
    #[test]
    fn evidence_checks_scope_before_existence_and_keeps_command_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        check_evidence_sources(&authority(), &root, &[], false).unwrap();
        for after in [false, true] {
            let error =
                check_evidence_sources(&authority(), &root, &["allowed/missing".into()], after)
                    .unwrap_err()
                    .to_string();
            assert_eq!(
                error,
                format!(
                    "evidence source does not exist{}: allowed/missing",
                    if after {
                        " after command execution"
                    } else {
                        ""
                    }
                )
            );
        }
        assert!(matches!(
            check_evidence_sources(&authority(), &root, &["outside".into()], false),
            Err(WorkspaceError::OutsideScope { .. })
        ));
        fs::create_dir(root.join("allowed")).unwrap();
        fs::write(root.join("allowed/source"), "ok").unwrap();
        check_evidence_sources(&authority(), &root, &["allowed/source".into()], true).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn links_cannot_escape_workspace_or_authority() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(root.join("allowed")).unwrap();
        fs::write(root.join("source"), "ok").unwrap();
        symlink(outside.path(), root.join("allowed/escape")).unwrap();
        assert!(matches!(
            safe_workspace_path(&root, "allowed/escape/missing", "path"),
            Err(WorkspaceError::Escape { .. })
        ));
        symlink(root.join("source"), root.join("allowed/source")).unwrap();
        assert!(matches!(
            check_evidence_sources(&authority(), &root, &["allowed/source".into()], false),
            Err(WorkspaceError::OutsideScope { .. })
        ));
    }
}

#[cfg(test)]
mod differential {
    use super::*;
    #[test]
    fn python_scope_and_path_fixtures() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let Some(expected) = crate::test_support::python(
            "skills/_shared/workspace_paths.py",
            r#"
p=pathlib.Path(sys.argv[3]); run={'authority':{'allowed_source_paths':['allowed']}}
results=[]
for value in ['', '/abs', '../outside', './a//b/', '.', 'allowed/new', 'elsewhere', 'allowed\0/x']:
    try:
        target,relative=m.safe_workspace_path(p,value,'path',ValueError)
        try:
            m.ensure_within_scope(run,p,target,'source',ValueError)
            error=None
        except ValueError as e:
            error=str(e)
        results.append([str(target),relative,error])
    except ValueError as e:
        results.append(str(e))
print(json.dumps(results))
"#,
            &[&root],
        ) else {
            return;
        };
        let run = serde_json::json!({"authority":{"allowed_source_paths":["allowed"]}});
        let actual: Vec<_> = [
            "",
            "/abs",
            "../outside",
            "./a//b/",
            ".",
            "allowed/new",
            "elsewhere",
            "allowed\0/x",
        ]
        .iter()
        .map(|value| match safe_workspace_path(&root, value, "path") {
            Err(error) => serde_json::json!(error.to_string()),
            Ok((target, relative)) => {
                let error = ensure_within_scope(&run, &root, &target, ScopeKind::Source)
                    .err()
                    .map(|e| e.to_string());
                serde_json::json!([target, relative, error])
            }
        })
        .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}

#[cfg(test)]
mod double_root_tests {
    use super::*;
    #[test]
    fn posix_double_slash_workspace_remains_a_distinct_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let double = PathBuf::from(format!("/{}", root.display()));
        let Some(expected) = crate::test_support::python(
            "skills/_shared/workspace_paths.py",
            r#"
try:
    m.safe_workspace_path(pathlib.Path(sys.argv[3]),'file','path',ValueError)
    print(json.dumps(None))
except ValueError as e:
    print(json.dumps(str(e)))
"#,
            &[&double],
        ) else {
            return;
        };
        assert_eq!(
            serde_json::json!(
                safe_workspace_path(&double, "file", "path")
                    .unwrap_err()
                    .to_string()
            ),
            expected
        );
    }
}
