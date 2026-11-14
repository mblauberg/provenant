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
    pub policy: &'a provenant_records::policy::Policy,
}

fn display_path(path: &Path, primary: &Path, home: &Path) -> String {
    if path
        .to_str()
        .is_none_or(|text| text.chars().any(char::is_control))
    {
        return "<unrepresentable path; inspect project configuration on host>".into();
    }
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
    if !provenant_confinement::permissions::profile_path_representable(&path) {
        return Err("unrepresentable_path");
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
) -> Result<Value, provenant_providers::ProviderError> {
    // Resolve against a copy of the current profile, then transfer only the
    // validated additions to the provider-owned requirements resolver.
    let mut candidate = provenant_confinement::permissions::ProviderProfile {
        spec: spec.profile.spec.clone(),
        observations: spec.profile.observations.clone(),
    };
    let read_start = candidate.spec.runtime_reads.len();
    let exec_start = candidate.spec.tool_exec_roots.len();
    let write_start = candidate.spec.tool_writes.len();
    let home = context.home;
    let cwd = context.cwd;
    let receipt = apply_to(&mut spec.env, &mut candidate, plan, context);
    spec.set_project_requirements(
        candidate.spec.runtime_reads[read_start..].to_vec(),
        candidate.spec.tool_exec_roots[exec_start..].to_vec(),
        candidate.spec.tool_writes[write_start..].to_vec(),
        receipt,
    );
    spec.complete_tool_requirements(plan, home, cwd)?;
    Ok(spec
        .project_requirements_receipt()
        .cloned()
        .unwrap_or(serde_json::Value::Null))
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
        .filter(|gitdir| context.writer || Some(*gitdir) != context.common_git)
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
                    Err(reason) => {
                        if let provenant_records::policy::Effect::Warn(message) =
                            context.policy.check(
                                "PROJECT_PATHS_RESOLVE_NONE",
                                provenant_records::policy::Facts::violation(format!(
                                    "configured project path {key} was omitted: {reason}"
                                )),
                            )
                        {
                            warnings.push(json!({
                                "code": if is_exec { "exec_root_ignored" } else { "shared_path_ignored" },
                                "policy_code": "PROJECT_PATHS_RESOLVE_NONE",
                                "source": format!("{:?}", context.policy.rule("PROJECT_PATHS_RESOLVE_NONE").source),
                                "key": key,
                                "reason": reason,
                                "policy_message": message,
                                "path": display_path(entry, primary, context.home),
                            }));
                        }
                    }
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
            &primary.join(".ssh"),
            &primary.join("protected"),
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
                policy: None,
                mode: AccessMode::ReadOnly,
                read_only: None,
                scratch: grant(root.join("scratch")),
                private_state: Some(grant(root.join("private"))),
                read_roots: vec![grant(cwd.clone())],
                protected_roots: vec![grant(primary.join("protected"))],
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
                ".ssh".into(),
                "protected".into(),
            ],
            ..Default::default()
        };
        for writer in [false, true] {
            let mut permissions = plan.permissions().clone();
            if writer {
                permissions.mode = AccessMode::WorktreeWrite(Box::new(serde_json::from_value(
                    json!({
                        "git_grants_digest": vec![7_u8; 32], "publishing": false,
                        "common_git_dir": common, "cargo_cache": {"path": root.join("cache"), "scope": "subtree"}
                    }),
                )?));
            }
            let mode_plan = ConfinedPlan::new(PlanRequest {
                scope: WorkScope::Project(ProjectId::from_bytes([2; 16])),
                codex: CodexPolicy::Clean,
                extensions: ExtensionPolicy::Deny,
                agy_state: None,
                permissions,
            })?;
            let mut profile = ProviderProfile {
                spec: ProviderProfileSpec::default(),
                observations: Default::default(),
            };
            let mut env = Vec::new();
            let record = apply_to(
                &mut env,
                &mut profile,
                &mode_plan,
                Context {
                    cwd: &cwd,
                    home: &home,
                    state_root: &root.join("state"),
                    common_git: Some(&common),
                    worktree_gitdir: Some(&admin),
                    writer,
                    settings: Some(&settings),
                    project: "example",
                    policy: &provenant_records::policy::Policy::default(),
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
            for (index, reason) in [
                (1, "outside_primary_checkout"),
                (2, "outside_primary_checkout"),
                (3, "outside_primary_checkout"),
                (4, "missing_or_unreadable"),
                (5, "credential_root"),
                (6, "protected_path"),
            ] {
                assert!(warnings.iter().any(|warning| warning["key"]
                    == format!("domains.lanes.projects.example.exec_roots[{index}]")
                    && warning["reason"] == reason
                    && warning["code"] == "exec_root_ignored"));
            }
            assert!(
                profile
                    .spec
                    .runtime_reads
                    .contains(&grant(primary.join(".local-tools")))
            );
            assert!(
                profile
                    .spec
                    .tool_writes
                    .iter()
                    .all(|write| !overlaps(&write.path, &primary.join(".local-tools")))
            );
            for warning in warnings {
                assert!(
                    warning["key"]
                        .as_str()
                        .ok_or("key")?
                        .starts_with("domains.lanes.projects.example.")
                );
            }
            if writer {
                let shared_warnings: Vec<_> = warnings
                    .iter()
                    .filter(|warning| {
                        warning["key"]
                            .as_str()
                            .is_some_and(|key| key.contains(".shared_paths["))
                    })
                    .collect();
                assert!(
                    shared_warnings
                        .iter()
                        .all(|warning| warning["code"] == "shared_path_ignored")
                );
                let reasons: Vec<_> = shared_warnings
                    .iter()
                    .filter_map(|warning| warning["reason"].as_str())
                    .collect();
                assert!(reasons.contains(&"git_directory"));
                assert!(reasons.contains(&"credential_root"));
                assert!(reasons.contains(&"missing_or_unreadable"));
                assert!(reasons.contains(&"executable_root"));
            }
        }
        let mut env = Vec::new();
        let mut profile = ProviderProfile {
            spec: ProviderProfileSpec::default(),
            observations: Default::default(),
        };
        apply_to(
            &mut env,
            &mut profile,
            &plan,
            Context {
                cwd: &primary,
                home: &home,
                state_root: &root.join("state"),
                common_git: Some(&common),
                worktree_gitdir: Some(&common),
                writer: false,
                settings: None,
                project: "example",
                policy: &provenant_records::policy::Policy::default(),
            },
        );
        assert!(
            !env.iter()
                .any(|(key, _)| key == "PROVENANT_WORKTREE_GITDIR")
        );
        assert!(!profile.spec.runtime_reads.contains(&grant(common.clone())));
        Ok(())
    }
    #[test]
    fn exec_roots_reject_files_and_overlapping_writes_in_both_modes()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let primary = root.join("repo");
        let home = root.join("home");
        let common = primary.join(".git");
        let tool = primary.join(".local-tools");
        for dir in [&home, &common, &tool] {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(primary.join("file"), b"not a directory")?;
        let grant = |path| PathGrant {
            path,
            scope: PathScope::Subtree,
        };
        let plan = ConfinedPlan::new(PlanRequest {
            scope: WorkScope::Project(ProjectId::from_bytes([2; 16])),
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
            permissions: PlanPermissions {
                policy: None,
                mode: AccessMode::ReadOnly,
                read_only: None,
                scratch: grant(root.join("scratch")),
                private_state: None,
                read_roots: vec![],
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
            exec_roots: vec![".local-tools".into(), "file".into()],
            ..Default::default()
        };
        for writer in [false, true] {
            let mut permissions = plan.permissions().clone();
            if writer {
                permissions.mode = AccessMode::WorktreeWrite(Box::new(serde_json::from_value(
                    json!({
                        "git_grants_digest": vec![7_u8; 32], "publishing": false,
                        "common_git_dir": common, "cargo_cache": {"path": root.join("cache"), "scope": "subtree"}
                    }),
                )?));
            }
            let mode_plan = ConfinedPlan::new(PlanRequest {
                scope: WorkScope::Project(ProjectId::from_bytes([2; 16])),
                codex: CodexPolicy::Clean,
                extensions: ExtensionPolicy::Deny,
                agy_state: None,
                permissions,
            })?;
            let mut profile = ProviderProfile {
                spec: ProviderProfileSpec {
                    tool_writes: vec![grant(tool.clone())],
                    ..Default::default()
                },
                observations: Default::default(),
            };
            let receipt = apply_to(
                &mut Vec::new(),
                &mut profile,
                &mode_plan,
                Context {
                    cwd: &primary,
                    home: &home,
                    state_root: &root,
                    common_git: Some(&common),
                    worktree_gitdir: None,
                    writer,
                    settings: Some(&settings),
                    project: "example",
                    policy: &provenant_records::policy::Policy::default(),
                },
            );
            assert!(profile.spec.tool_exec_roots.is_empty());
            let warnings = receipt["warnings"].as_array().ok_or("warnings")?;
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning["reason"] == "writable_root")
            );
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning["reason"] == "not_directory")
            );
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn exec_roots_omit_unrepresentable_canonical_targets() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let primary = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(&primary)?;
        std::fs::create_dir_all(&home)?;
        let target = primary.join(std::ffi::OsString::from_vec(b"tools-\xff".to_vec()));
        std::fs::create_dir(&target)?;
        std::os::unix::fs::symlink(&target, primary.join("tools-link"))?;
        let grant = |path| PathGrant {
            path,
            scope: PathScope::Subtree,
        };
        let plan = ConfinedPlan::new(PlanRequest {
            scope: WorkScope::Project(ProjectId::from_bytes([2; 16])),
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
            permissions: PlanPermissions {
                policy: None,
                mode: AccessMode::ReadOnly,
                read_only: None,
                scratch: grant(root.join("scratch")),
                private_state: None,
                read_roots: vec![],
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
            exec_roots: vec!["tools-link".into()],
            ..Default::default()
        };
        let mut profile = ProviderProfile {
            spec: ProviderProfileSpec::default(),
            observations: Default::default(),
        };
        let receipt = apply_to(
            &mut Vec::new(),
            &mut profile,
            &plan,
            Context {
                cwd: &primary,
                home: &home,
                state_root: &root,
                common_git: None,
                worktree_gitdir: None,
                writer: false,
                settings: Some(&settings),
                project: "example",
                policy: &provenant_records::policy::Policy::default(),
            },
        );
        assert!(profile.spec.tool_exec_roots.is_empty());
        assert_eq!(receipt["warnings"][0]["reason"], "unrepresentable_path");
        assert_eq!(
            receipt["warnings"][0]["key"],
            "domains.lanes.projects.example.exec_roots[0]"
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn exec_roots_native_read_exec_and_deny_write_in_both_modes()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::permissions::{
            BackendContext, WorktreeWriteAuthority, system_backend,
        };
        use provenant_os::exec::{
            ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
        };
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let primary = root.join("repo");
        let home = root.join("home");
        let common = primary.join(".git");
        let tools = primary.join(".local-tools");
        let scratch = root.join("scratch");
        for path in [&home, &common, &tools, &scratch] {
            std::fs::create_dir_all(path)?;
        }
        let script = tools.join("tool");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n")?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        let executor = SystemExecutor::new(2)?;
        let cancel = provenant_core::CancelToken::new();
        if let Err(error) = system_backend(&BackendContext {
            executor: &executor,
            cancel: &cancel,
            env: &[],
        }) {
            provenant_test_support::skip_sandbox_dependent_test(format!(
                "native sandbox unavailable: {error}"
            ))?;
            return Ok(());
        }
        let grant = |path| PathGrant {
            path,
            scope: PathScope::Subtree,
        };
        let settings = crate::domains::ProjectLanesConfig {
            exec_roots: vec![".local-tools".into()],
            ..Default::default()
        };
        for writer in [false, true] {
            let mode = if writer {
                AccessMode::WorktreeWrite(Box::new(
                    serde_json::from_value::<WorktreeWriteAuthority>(json!({
                        "git_grants_digest": vec![7_u8; 32], "publishing": false,
                        "common_git_dir": common, "cargo_cache": {"path": scratch.join("cache"), "scope": "subtree"}
                    }))?,
                ))
            } else {
                AccessMode::ReadOnly
            };
            let plan = ConfinedPlan::new(PlanRequest {
                scope: WorkScope::Project(ProjectId::from_bytes([3; 16])),
                codex: CodexPolicy::Clean,
                extensions: ExtensionPolicy::Deny,
                agy_state: None,
                permissions: PlanPermissions {
                    policy: None,
                    mode,
                    read_only: None,
                    scratch: grant(scratch.clone()),
                    private_state: None,
                    read_roots: vec![],
                    protected_roots: vec![],
                    secret_roots: vec![],
                    selected_authentication: None,
                    include_continuation: false,
                    write_roots: vec![],
                    network: NetworkAccess::Provider,
                    nested_confinement: false,
                },
            })?;
            let mut profile = ProviderProfile {
                spec: ProviderProfileSpec {
                    allow_process_fork: true,
                    tool_exec_roots: vec![grant("/bin".into()), grant("/usr/bin".into())],
                    ..Default::default()
                },
                observations: Default::default(),
            };
            let receipt = apply_to(
                &mut Vec::new(),
                &mut profile,
                &plan,
                Context {
                    cwd: &primary,
                    home: &home,
                    state_root: &root,
                    common_git: Some(&common),
                    worktree_gitdir: None,
                    writer,
                    settings: Some(&settings),
                    project: "example",
                    policy: &provenant_records::policy::Policy::default(),
                },
            );
            assert_eq!(receipt["exec_roots"].as_array().ok_or("roots")?.len(), 1);
            let sbpl = provenant_confinement::permissions::snapshot::render(
                plan.permissions(),
                &profile,
                &home,
            )?;
            let argv = [
                "/usr/bin/sandbox-exec".into(),
                "-p".into(),
                sbpl.into(),
                "/bin/sh".into(),
                "-c".into(),
                r#"test -r "$1" && "$1" && ! /usr/bin/touch "$2""#.into(),
                "probe".into(),
                script.as_os_str().to_owned(),
                tools.join("write-probe").into_os_string(),
            ];
            let mut options = ExecOptions::new(&argv, &root, Duration::from_secs(10));
            options.stderr = StreamPolicy::Capture { limit: 2048 };
            let result = executor.execute(options)?;
            assert_eq!(
                result.termination,
                Termination::Exit(0),
                "writer={writer}, writable={}, stderr={}",
                tools.join("write-probe").exists(),
                String::from_utf8_lossy(&result.stderr.bytes)
            );
            assert!(!tools.join("write-probe").exists());
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn configured_shared_path_allows_lane_lock_contended_by_host() -> Result<(), String> {
        provenant_test_support::skip_sandbox_dependent_test(
            "native Seatbelt lock contention requires macOS",
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn configured_shared_path_allows_lane_lock_contended_by_host()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::permissions::{
            BackendContext, WorktreeWriteAuthority, system_backend,
        };
        use provenant_os::exec::{
            ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
        };
        use std::ffi::OsString;
        use std::time::Duration;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let primary = root.join("repo");
        let home = root.join("home");
        let common = primary.join(".git");
        let admin = common.join("worktrees/writer");
        let cwd = root.join("writer");
        let shared = primary.join(".agent-run/locks");
        let scratch = root.join("scratch");
        let state = root.join("state");
        for path in [&home, &admin, &cwd, &shared, &scratch, &state] {
            std::fs::create_dir_all(path)?;
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
        let executor = SystemExecutor::new(2)?;
        let cancel = provenant_core::CancelToken::new();
        let env: Vec<(OsString, OsString)> = Vec::new();
        if let Err(error) = system_backend(&BackendContext {
            executor: &executor,
            cancel: &cancel,
            env: &env,
        }) {
            provenant_test_support::skip_sandbox_dependent_test(format!(
                "native sandbox unavailable: {error}"
            ))?;
            return Ok(());
        }
        let mut roots = provenant_core::RootInputs::new(home.clone(), primary.clone());
        roots.state_dir = Some(state.clone());
        let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
        std::fs::write(
            state.join("config.toml"),
            "schema_version = 1\n[domains.lanes.projects.example]\nshared_paths = ['.agent-run/locks']\n",
        )?;
        let config = provenant_config::read_with_domains(&roots, &crate::domains::SECTIONS)?;
        let settings = config
            .domains
            .lanes
            .projects
            .get("example")
            .ok_or("project settings")?;
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
                policy: None,
                mode: AccessMode::WorktreeWrite(Box::new(serde_json::from_value::<
                    WorktreeWriteAuthority,
                >(json!({
                    "git_grants_digest": vec![7_u8; 32], "publishing": false,
                    "common_git_dir": common, "cargo_cache": {"path": scratch.join("cache"), "scope": "subtree"}
                }))?)),
                read_only: None,
                scratch: grant(scratch.clone()),
                private_state: None,
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
        let mut profile = ProviderProfile {
            spec: ProviderProfileSpec {
                allow_process_fork: true,
                tool_exec_roots: vec![grant("/usr/bin".into()), grant("/bin".into())],
                ..Default::default()
            },
            observations: Default::default(),
        };
        let record = apply_to(
            &mut Vec::new(),
            &mut profile,
            &plan,
            Context {
                cwd: &cwd,
                home: &home,
                state_root: &state,
                common_git: Some(&common),
                worktree_gitdir: Some(&admin),
                writer: true,
                settings: Some(settings),
                project: "example",
                policy: &provenant_records::policy::Policy::default(),
            },
        );
        assert_eq!(record["shared_paths"][0]["path"], "./.agent-run/locks");
        let sbpl = provenant_confinement::permissions::snapshot::render(
            plan.permissions(),
            &profile,
            &home,
        )?;
        let lock_dir = provenant_os::fs::Directory::open_anchor(&shared)?;
        let release = lock_dir.try_lock(
            std::ffi::OsStr::new("release.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        let lock = shared.join("lane.lock");
        let ready = shared.join("ready");
        let started = shared.join("host-started");
        let acquired = shared.join("host-acquired");
        let release_path = shared.join("release.lock");
        std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
            let lane = scope.spawn(|| {
                let argv = [OsString::from("/usr/bin/sandbox-exec"), "-p".into(), sbpl.into(), "/usr/bin/perl".into(), "-e".into(),
                    "open(my $f, '>>', $ARGV[0]) or die $!; flock($f, 2) or die $!; open(my $r, '>', $ARGV[1]) or die $!; close($r); open(my $g, '>>', $ARGV[2]) or die $!; flock($g, 2) or die $!;".into(),
                    lock.as_os_str().to_owned(), ready.as_os_str().to_owned(), release_path.as_os_str().to_owned()];
                let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(20));
                options.stderr = StreamPolicy::Capture { limit: 1024 };
                executor.execute(options)
            });
            provenant_test_support::wait_for(
                "lane lock",
                Duration::from_secs(10),
                || ready.exists(),
                |ready| *ready,
            )?;
            assert!(
                lock_dir
                    .try_lock(
                        std::ffi::OsStr::new("lane.lock"),
                        provenant_os::fs::LockMode::Exclusive
                    )
                    .is_err()
            );
            let host = scope.spawn(|| {
                let argv = [OsString::from("/usr/bin/perl"), "-e".into(),
                    "open(my $f, '>>', $ARGV[0]) or die $!; open(my $s, '>', $ARGV[1]) or die $!; close($s); flock($f, 2) or die $!; open(my $a, '>', $ARGV[2]) or die $!;".into(),
                    lock.as_os_str().to_owned(), started.as_os_str().to_owned(), acquired.as_os_str().to_owned()];
                let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(20));
                options.stderr = StreamPolicy::Capture { limit: 1024 };
                executor.execute(options)
            });
            provenant_test_support::wait_for(
                "host contender",
                Duration::from_secs(10),
                || started.exists(),
                |started| *started,
            )?;
            assert!(!acquired.exists());
            drop(release);
            let lane = lane.join().map_err(|_| "lane thread")??;
            let host = host.join().map_err(|_| "host thread")??;
            assert_eq!(lane.termination, Termination::Exit(0), "{lane:?}");
            assert_eq!(host.termination, Termination::Exit(0), "{host:?}");
            assert!(acquired.exists());
            Ok(())
        })?;
        Ok(())
    }
}
