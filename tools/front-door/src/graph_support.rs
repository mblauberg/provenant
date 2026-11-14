//! Shared graph launch helpers: route lowering and confined project roots.
use provenant_fabric_runtime::read_roots::ReadRoots;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(crate) use provenant_cli::fabric::graph_relay::tier_route;

/// Repository registration discovery has three outcomes so a damaged or
/// unavailable inventory cannot masquerade as a one-worktree repository.
pub(crate) enum ProjectWorktrees {
    Registered(Vec<PathBuf>),
    NotRepository,
    Unavailable(String),
}

/// Whether a path has the structural markers of a working or bare repository.
/// This probe only classifies a failed listing; Git remains the inventory owner.
fn has_repository_markers(directory: &Path) -> Result<bool, String> {
    use std::io::ErrorKind;
    fn present(path: &Path) -> Result<bool, String> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }
    for parent in directory.ancestors() {
        if present(&parent.join(".git"))? {
            return Ok(true);
        }
        if present(&parent.join("HEAD"))?
            && present(&parent.join("objects"))?
            && present(&parent.join("refs"))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Discover registrations main-checkout first. No repository is a supported
/// state; an incomplete inventory is a distinct warning state.
pub(crate) fn project_worktrees(directory: &Path) -> ProjectWorktrees {
    match has_repository_markers(directory) {
        Ok(false) => return ProjectWorktrees::NotRepository,
        Err(reason) => return ProjectWorktrees::Unavailable(reason),
        Ok(true) => {}
    }
    let read = || -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
        let git = provenant_os::git::git_executable().ok_or("Git executable unavailable")?;
        let environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                !key.to_str()
                    .is_some_and(|key| crate::fabric::GIT_REDIRECTS.contains(&key))
            })
            .chain([("GIT_TERMINAL_PROMPT".into(), "0".into())])
            .collect();
        let executor = provenant_os::exec::SystemExecutor::new(1)?;
        Ok(
            provenant_confinement::worktrees::RepoFacts::default().discover_worktree_paths(
                directory,
                &git,
                &environment,
                &executor,
                std::time::Duration::from_secs(60),
                &provenant_core::CancelToken::new(),
            )?,
        )
    };
    match read() {
        Ok(paths) => ProjectWorktrees::Registered(paths),
        Err(error) => ProjectWorktrees::Unavailable(error.to_string()),
    }
}

/// Whether a read root is exactly one `${parameter}`, with nothing around it.
pub(crate) fn bare_placeholder(text: &str) -> bool {
    text.strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .is_some_and(|name| !name.is_empty() && !name.contains(['$', '{', '}']))
}

/// A planner pattern names a read root by a closed-list parameter. The planner
/// can pick any listed value, so every one is confined to the project's
/// registered worktrees and allow-list now, exactly like a literal root, and a
/// root that is not exactly one parameter is refused. Each value is replaced by
/// the canonical path it confined to, so a link retargeted after admission
/// cannot move the grant, and the planner's catalogue lists the same paths.
pub(crate) fn confine_pattern_roots(node: &mut Value, policy: &ReadRoots) -> Result<(), String> {
    let Some(patterns) = node
        .pointer_mut("/plan/patterns")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    let mut renamed: Vec<(String, String)> = Vec::new();
    for pattern in patterns {
        let name = pattern
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let mut parameters = Vec::new();
        fn collect_roots<'a>(value: &'a Value, roots: &mut Vec<&'a Value>) {
            match value {
                Value::Object(map) => {
                    if let Some(items) = map.get("read_roots").and_then(Value::as_array) {
                        roots.extend(items);
                    }
                    for (key, child) in map {
                        if key != "read_roots" {
                            collect_roots(child, roots);
                        }
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        collect_roots(item, roots);
                    }
                }
                _ => {}
            }
        }
        let mut roots = Vec::new();
        collect_roots(&pattern["nodes"], &mut roots);
        for root in roots {
            let text = root.as_str().ok_or("read_roots entries must be strings")?;
            if !text.contains("${") {
                continue;
            }
            if !bare_placeholder(text) {
                return Err(format!(
                    "plan pattern {name}: read root {text} must be exactly one parameter"
                ));
            }
            parameters.push(text[2..text.len() - 1].to_owned());
        }
        for parameter in parameters {
            let values = pattern
                .pointer_mut(&format!("/parameters/{parameter}/enum"))
                .and_then(Value::as_array_mut)
                .filter(|values| !values.is_empty())
                .ok_or_else(|| {
                    format!("plan pattern {name}: read root ${{{parameter}}} needs a closed list")
                })?;
            for value in values {
                let given = value
                    .as_str()
                    .ok_or("read root values must be strings")?
                    .to_owned();
                if !Path::new(&given).is_absolute() {
                    return Err(format!(
                        "plan pattern {name}: read root {given} must be absolute"
                    ));
                }
                let canonical = policy
                    .confine(Path::new(&given))?
                    .to_str()
                    .ok_or("read root is not UTF-8")?
                    .to_owned();
                if canonical != given {
                    renamed.push((given, canonical.clone()));
                    *value = json!(canonical);
                }
            }
            // A default that named a link follows its value.
            if let Some(default) = pattern.pointer_mut(&format!("/parameters/{parameter}/default"))
                && let Some((_, canonical)) = default
                    .as_str()
                    .and_then(|text| renamed.iter().find(|(given, _)| given == text))
            {
                *default = json!(canonical);
            }
        }
    }
    // The planner reads the same canonical paths the catalogue will accept.
    if let Some(prompt) = node.get("prompt").and_then(Value::as_str) {
        let mut text = prompt.to_owned();
        for (given, canonical) in &renamed {
            text = replace_path(&text, given, canonical);
        }
        node["prompt"] = json!(text);
    }
    Ok(())
}

/// Replace `from` with `to` where `from` is a whole path, not the tail of a
/// longer one such as `/var/x` inside `/private/var/x`.
fn replace_path(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(from) {
        let before = rest[..at].chars().next_back().or(out.chars().next_back());
        let after = rest[at + from.len()..].chars().next();
        let inside = |c: char| c.is_alphanumeric() || matches!(c, '/' | '_' | '-');
        out.push_str(&rest[..at]);
        if before.is_some_and(|c| inside(c) || c == '.') || after.is_some_and(inside) {
            out.push_str(from);
        } else {
            out.push_str(to);
        }
        rest = &rest[at + from.len()..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn format_family_group(group_index: usize) -> String {
    format!("group-{group_index}")
}

#[cfg(test)]
mod discovery_tests {
    #[test]
    fn repository_discovery_classification_uses_git_markers() {
        use super::*;
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("checkout/src");
        std::fs::create_dir_all(&worktree).unwrap();
        assert!(!has_repository_markers(&worktree).unwrap());
        let git = temp.path().join("checkout/.git");
        std::fs::create_dir(&git).unwrap();
        assert!(has_repository_markers(&worktree).unwrap());
        std::fs::remove_dir(&git).unwrap();
        let bare = temp.path().join("bare");
        std::fs::create_dir_all(bare.join("objects")).unwrap();
        std::fs::create_dir(bare.join("refs")).unwrap();
        std::fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(has_repository_markers(&bare).unwrap());
        let linked = temp.path().join("linked");
        std::fs::create_dir(&linked).unwrap();
        symlink(git, linked.join(".git")).unwrap();
        assert!(has_repository_markers(&linked).unwrap());
    }
}
