//! Project-owned access additions. Resolve once on the host; use these same
//! facts for the compiled profile and the receipt.
use super::*;
use serde_json::{Value, json};

pub(super) struct Context<'a> {
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub state_root: &'a Path,
    pub common_git: Option<&'a Path>,
    pub worktree_gitdir: Option<&'a Path>,
    pub writer: bool,
    pub settings: Option<&'a crate::domains::ProjectLanesConfig>,
    pub project: &'a str,
}

fn display_path(path: &Path, primary: &Path, home: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(primary) {
        return format!("./{}", relative.display());
    }
    if let Ok(relative) = path.strip_prefix(home) {
        return format!("~/{}", relative.display());
    }
    // A receipt can leave this host. An external path has no portable anchor.
    "<external path; inspect project configuration on host>".into()
}

fn overlaps(root: &Path, other: &Path) -> bool {
    root.starts_with(other) || other.starts_with(root)
}

fn resolve(
    entry: &Path,
    exec: bool,
    primary: &Path,
    context: &Context<'_>,
    plan: &ConfinedPlan,
) -> Result<PathGrant, &'static str> {
    if exec
        && (entry.is_absolute()
            || entry
                .components()
                .any(|c| c == std::path::Component::ParentDir))
    {
        return Err("outside_primary_checkout");
    }
    let path = if let Ok(relative) = entry.strip_prefix("~") {
        if exec {
            return Err("outside_primary_checkout");
        }
        context.home.join(relative)
    } else {
        primary.join(entry)
    };
    let path = std::fs::canonicalize(path).map_err(|_| "missing_or_unreadable")?;
    if exec && !path.starts_with(primary) {
        return Err("outside_primary_checkout");
    }
    if exec && !path.is_dir() {
        return Err("not_directory");
    }
    if path.components().any(|c| c.as_os_str() == ".git")
        || context.common_git.is_some_and(|git| overlaps(&path, git))
        || context
            .worktree_gitdir
            .is_some_and(|git| overlaps(&path, git))
    {
        return Err("git_directory");
    }
    if provenant_confinement::refuses_lane_root(&path, exec, Some(context.home)) {
        return Err("credential_root");
    }
    if plan
        .permissions()
        .protected_roots
        .iter()
        .any(|root| overlaps(&path, &root.path))
    {
        return Err("protected_path");
    }
    Ok(PathGrant {
        scope: if path.is_dir() {
            PathScope::Subtree
        } else {
            PathScope::Exact
        },
        path,
    })
}

pub(super) fn apply(
    spec: &mut provenant_providers::LaunchSpec,
    plan: &ConfinedPlan,
    context: Context<'_>,
) -> Value {
    apply_to(&mut spec.env, &mut spec.profile, plan, context)
}

fn apply_to(
    env: &mut Vec<(std::ffi::OsString, std::ffi::OsString)>,
    profile: &mut provenant_confinement::permissions::ProviderProfile,
    plan: &ConfinedPlan,
    context: Context<'_>,
) -> Value {
    let primary = context
        .common_git
        .filter(|git| git.file_name().is_some_and(|name| name == ".git"))
        .and_then(Path::parent)
        .unwrap_or(context.cwd);
    env.push((
        "PROVENANT_PRIMARY_CHECKOUT".into(),
        primary.as_os_str().to_owned(),
    ));
    if let Some(gitdir) = context
        .worktree_gitdir
        .filter(|git| context.writer || Some(*git) != context.common_git)
    {
        env.push((
            "PROVENANT_WORKTREE_GITDIR".into(),
            gitdir.as_os_str().to_owned(),
        ));
        profile.spec.runtime_reads.push(PathGrant {
            path: gitdir.to_owned(),
            scope: PathScope::Subtree,
        });
    }
    let mut shared = Vec::new();
    let mut exec = Vec::new();
    let mut warnings = Vec::new();
    if let Some(settings) = context.settings {
        // Exec roots must remain read-only, even when two settings overlap.
        for (is_exec, entries) in [
            (true, &settings.exec_roots),
            (false, &settings.shared_paths),
        ] {
            if !is_exec && !context.writer {
                continue;
            }
            let field = if is_exec {
                "exec_roots"
            } else {
                "shared_paths"
            };
            for (index, entry) in entries.iter().enumerate() {
                let key = format!(
                    "domains.lanes.projects.{}.{field}[{index}]",
                    context.project
                );
                let result = resolve(entry, is_exec, primary, &context, plan).and_then(|grant| {
                    if !is_exec {
                        let write = provenant_confinement::permissions::WriteGrant {
                            source: provenant_confinement::permissions::WriteSource::ToolWrite,
                            path: grant.path.clone(),
                            scope: grant.scope,
                        };
                        if !host_indexes_protected(context.state_root, std::slice::from_ref(&write))
                            || !target_seeds_protected(context.state_root, &[write])
                        {
                            return Err("host_state");
                        }
                    }
                    if !is_exec
                        && profile
                            .spec
                            .tool_exec_roots
                            .iter()
                            .any(|root| overlaps(&grant.path, &root.path))
                    {
                        return Err("executable_root");
                    }
                    if is_exec
                        && provenant_confinement::write_grants(plan.permissions(), profile)
                            .iter()
                            .any(|root| overlaps(&grant.path, &root.path))
                    {
                        return Err("writable_root");
                    }
                    Ok(grant)
                });
                match result {
                    Ok(grant) => {
                        let record = json!({"path": display_path(&grant.path, primary, context.home), "source": key});
                        if is_exec {
                            profile.spec.runtime_reads.push(grant.clone());
                            profile.spec.tool_exec_roots.push(grant);
                            exec.push(record);
                        } else {
                            profile.spec.tool_writes.push(grant);
                            shared.push(record);
                        }
                    }
                    Err(reason) => warnings.push(json!({
                        "code": if is_exec { "exec_root_ignored" } else { "shared_path_ignored" },
                        "key": key,
                        "reason": reason,
                        "path": display_path(entry, primary, context.home),
                    })),
                }
            }
        }
    }
    json!({"shared_paths": shared, "exec_roots": exec, "warnings": warnings})
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_confinement::permissions::{NetworkAccess, ProviderProfile, ProviderProfileSpec};

    #[test]
    fn project_paths_are_resolved_once_and_receipt_matches_applied_grants()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?;
        let primary = root.join("repo");
        let home = root.join("home");
        let common = primary.join(".git");
        let admin = common.join("worktrees/writer");
        let cwd = primary.join(".worktrees/writer");
        for path in [
            &home,
            &admin,
            &cwd,
            &primary.join(".agent-run/locks"),
            &primary.join(".local-tools"),
            &home.join(".ssh"),
            &home.join("shared"),
        ] {
            std::fs::create_dir_all(path)?;
        }
        std::os::unix::fs::symlink(home.join("shared"), primary.join("escape"))?;
        let grant = |path: PathBuf| PathGrant {
            path,
            scope: PathScope::Subtree,
        };
        let plan = ConfinedPlan::new(PlanRequest {
            scope: WorkScope::Project(ProjectId::from_bytes([1; 16])),
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
            permissions: PlanPermissions {
                mode: AccessMode::ReadOnly,
                read_only: None,
                scratch: grant(root.join("scratch")),
                private_state: Some(grant(root.join("private"))),
                read_roots: vec![grant(cwd.clone())],
                protected_roots: vec![],
                secret_roots: vec![],
                selected_authentication: None,
                include_continuation: false,
                write_roots: vec![],
                network: NetworkAccess::Provider,
                nested_confinement: false,
            },
        })?;
        let settings = crate::domains::ProjectLanesConfig {
            shared_paths: vec![
                ".agent-run/locks".into(),
                "~/shared".into(),
                ".git".into(),
                "~/.ssh".into(),
                "missing".into(),
                ".local-tools".into(),
            ],
            exec_roots: vec![
                ".local-tools".into(),
                "../home".into(),
                home.clone(),
                "escape".into(),
                "missing".into(),
            ],
            ..Default::default()
        };
        for writer in [false, true] {
            let mut profile = ProviderProfile {
                spec: ProviderProfileSpec::default(),
                observations: Default::default(),
            };
            let mut env = Vec::new();
            let record = apply_to(
                &mut env,
                &mut profile,
                &plan,
                Context {
                    cwd: &cwd,
                    home: &home,
                    state_root: &root.join("state"),
                    common_git: Some(&common),
                    worktree_gitdir: Some(&admin),
                    writer,
                    settings: Some(&settings),
                    project: "example",
                },
            );
            assert_eq!(
                profile.spec.tool_exec_roots,
                [grant(primary.join(".local-tools"))]
            );
            assert_eq!(record["exec_roots"][0]["path"], "./.local-tools");
            assert_eq!(profile.spec.tool_writes.len(), if writer { 2 } else { 0 });
            assert_eq!(
                record["shared_paths"].as_array().ok_or("array")?.len(),
                profile.spec.tool_writes.len()
            );
            assert!(env.contains(&(
                "PROVENANT_PRIMARY_CHECKOUT".into(),
                primary.as_os_str().to_owned()
            )));
            assert!(env.contains(&(
                "PROVENANT_WORKTREE_GITDIR".into(),
                admin.as_os_str().to_owned()
            )));
            assert!(profile.spec.runtime_reads.contains(&grant(admin.clone())));
            let serialized = serde_json::to_string(&record)?;
            assert!(!serialized.contains(root.to_str().ok_or("utf8")?));
            let warnings = record["warnings"].as_array().ok_or("warnings")?;
            assert_eq!(warnings.len(), if writer { 8 } else { 4 });
            for warning in warnings {
                assert!(
                    warning["key"]
                        .as_str()
                        .ok_or("key")?
                        .starts_with("domains.lanes.projects.example.")
                );
            }
            if writer {
                let reasons: Vec<_> = warnings
                    .iter()
                    .filter_map(|w| w["reason"].as_str())
                    .collect();
                assert!(reasons.contains(&"git_directory"));
                assert!(reasons.contains(&"credential_root"));
                assert!(reasons.contains(&"missing_or_unreadable"));
                assert!(reasons.contains(&"executable_root"));
            }
        }
        Ok(())
    }
}
