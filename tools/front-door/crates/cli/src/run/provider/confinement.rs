use super::*;

/// Bind declared lane capabilities to the provider's confinement profile.
pub(in crate::run) fn apply_lane_capabilities(
    profile: &mut provenant_confinement::permissions::ProviderProfileSpec,
    access: &run::LaneAccess,
) {
    profile.allow_database_ipc = access.database_ipc;
}

/// The host directory under which every graph node's materialised upstream
/// results live, one subdirectory per project, graph and node.
pub fn graph_inputs_directory(state_root: &Path) -> PathBuf {
    state_root.join("fabric").join("graph-inputs")
}

/// The one inputs directory a graph node may read: its own, named by a digest
/// of its project, graph and node. The digest keeps names that a
/// case-insensitive filesystem would merge (`Review` and `review`), or that
/// are not path-safe, in separate directories. `None` when the project is not
/// a single path component.
pub fn graph_node_inputs_directory(
    state_root: &Path,
    project: impl std::fmt::Display,
    graph_id: &str,
    node_id: &str,
) -> Option<PathBuf> {
    let project = project.to_string();
    if matches!(project.as_str(), "" | "." | "..") || project.contains(['/', '\\', '\0']) {
        return None;
    }
    // Length-prefixed, so no two tuples share an encoding.
    let mut tuple = b"provenant graph inputs v1".to_vec();
    for part in [project.as_str(), graph_id, node_id] {
        tuple.extend_from_slice(&(part.len() as u64).to_be_bytes());
        tuple.extend_from_slice(part.as_bytes());
    }
    let digest = provenant_records::CanonicalV1::file_digest(&tuple);
    Some(
        graph_inputs_directory(state_root)
            .join(project)
            .join(provenant_core::hex(digest.bytes())),
    )
}

pub(super) struct ConfinementRoots<'a> {
    pub(super) host_roots: Option<&'a provenant_core::Roots>,
    pub(super) cwd: &'a Path,
    pub(super) home: &'a Path,
    pub(super) state: &'a Path,
    pub(super) credential_roots: &'a [PathGrant],
    pub(super) github_roots: &'a [PathGrant],
    pub(super) protected_worktrees: &'a [PathGrant],
    pub(super) scratch: &'a Path,
    pub(super) private_state: &'a Path,
    pub(super) assets: &'a Path,
    pub(super) prompt_file: Option<&'a Path>,
}

/// Name progress when the plan has a private-state grant.
pub(super) fn handoff_writable(permissions: &PlanPermissions) -> bool {
    permissions.private_state.is_some()
}

/// Point the lane's temporary-directory variables at the short alias of
/// `scratch`, leaving every other path, and every grant, on the real one.
pub(super) fn alias_temporary_env(
    env: &mut [(std::ffi::OsString, std::ffi::OsString)],
    scratch: &Path,
    alias: &Path,
) {
    for (key, value) in env {
        if ["TMPDIR", "TMPPREFIX", "CLAUDE_CODE_TMPDIR"]
            .iter()
            .any(|name| key == name)
            && let Ok(rest) = Path::new(value).strip_prefix(scratch)
        {
            *value = if rest.as_os_str().is_empty() {
                alias.as_os_str().to_owned()
            } else {
                alias.join(rest).into_os_string()
            };
        }
    }
}

pub(super) fn host_indexes_protected(
    state: &Path,
    grants: &[provenant_confinement::permissions::WriteGrant],
) -> bool {
    let resolve = |path: &Path| {
        provenant_core::PathResolver::resolve_existing_ancestor(
            &provenant_os::fs::SystemPathResolver,
            path,
        )
        .ok()
    };
    [
        "fabric/fabric.sqlite3",
        "fabric/host.sqlite3",
        run::WORK_INDEX_FILE,
    ]
    .iter()
    .flat_map(|file| {
        ["", "-wal", "-shm", "-journal", ".lock"].map(|suffix| format!("{file}{suffix}"))
    })
    .all(|file| {
        let Some(path) = resolve(&state.join(file)) else {
            return false;
        };
        grants.iter().all(|grant| {
            resolve(&grant.path).is_some_and(|root| match grant.scope {
                PathScope::Exact => root != path && !(root.is_dir() && path.starts_with(root)),
                PathScope::Subtree => !path.starts_with(root),
            })
        })
    })
}

pub(super) fn target_seeds_protected(
    state: &Path,
    grants: &[provenant_confinement::permissions::WriteGrant],
) -> bool {
    let resolve = |path: &Path| {
        provenant_core::PathResolver::resolve_existing_ancestor(
            &provenant_os::fs::SystemPathResolver,
            path,
        )
        .ok()
    };
    let Some(store) = resolve(&run::target_seed::store_path(state)) else {
        return false;
    };
    grants.iter().all(|grant| {
        resolve(&grant.path).is_some_and(|root| {
            !root.starts_with(&store)
                && !(store.starts_with(&root)
                    && (grant.scope == PathScope::Subtree || root.is_dir()))
        })
    })
}

/// Read access to the primary checkout's project scratch (`.provenant/scratch`, or
/// `.agent-run`), where briefs and graph documents live. Git excludes it, so a
/// writer's linked worktree does not hold it. Final-component links are refused;
/// parent links may only resolve within the canonical primary checkout.
pub(super) fn project_scratch_reads(project: &Path) -> Vec<PathGrant> {
    let Ok(project) = std::fs::canonicalize(project) else {
        return Vec::new();
    };
    [".provenant/scratch", ".agent-run"]
        .map(|name| project.join(name))
        .into_iter()
        .filter(|path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()))
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .filter(|path| path.starts_with(&project))
        .map(|path| PathGrant {
            path,
            scope: PathScope::Subtree,
        })
        .collect()
}

/// Dispatch write roots, plus project-resolved shared run state for writers only.
pub(super) fn write_roots(
    access: &run::LaneAccess,
    host_writes: &provenant_confinement::run_config::RunConfig,
    writer: bool,
    project: &str,
) -> Vec<PathGrant> {
    access
        .write_roots
        .iter()
        .map(PathBuf::as_path)
        .chain(
            writer
                .then(|| host_writes.writer_write_roots_for(project))
                .into_iter()
                .flatten(),
        )
        .map(|path| PathGrant {
            path: path.to_owned(),
            scope: PathScope::Subtree,
        })
        .collect()
}

/// Credential discovery belongs to a selected launch, never adapter eligibility.
/// Preflight still validates the fixed authority roots without walking the home.
pub(super) enum InventoryContext<'a> {
    Preflight,
    Launch(&'a run::TaskRequest, &'a str),
    #[cfg(any(test, feature = "fixture"))]
    Sandboxed,
}

// Composition edge keeps launch authority and inventory mode explicit.
#[allow(clippy::too_many_arguments)]
pub(super) fn confinement_request(
    project_id: ProjectId,
    roots: ConfinementRoots<'_>,
    writer: Option<&run::WriterLaunch>,
    access: &run::LaneAccess,
    host_writes: &provenant_confinement::run_config::RunConfig,
    adapter: &str,
    publishing: bool,
    sandbox_context: InventoryContext<'_>,
) -> Result<PlanRequest, provenant_confinement::permissions::PermissionError> {
    let mode = match writer {
        Some(writer) => {
            let mut authority =
                WorktreeWriteAuthority::for_worktree(&writer.worktree, &writer.branch, roots.home)?;
            authority.set_publishing(publishing);
            AccessMode::WorktreeWrite(Box::new(authority))
        }
        None => AccessMode::ReadOnly,
    };
    let grant = |path: &Path| PathGrant {
        path: path.to_owned(),
        scope: PathScope::Subtree,
    };
    let fixture_roots;
    let host_roots = match roots.host_roots {
        Some(host) => host,
        None => {
            let mut inputs =
                provenant_core::RootInputs::new(roots.home.to_owned(), roots.cwd.to_owned());
            inputs.state_dir = Some(roots.state.to_owned());
            fixture_roots = inputs
                .resolve(&provenant_os::fs::SystemPathResolver)
                .map_err(|_| {
                    provenant_confinement::permissions::PermissionError::AnchorMismatchAt {
                        path: roots.state.to_owned(),
                        stage: "host state root binding",
                    }
                })?;
            &fixture_roots
        }
    };
    let sandboxed = match sandbox_context {
        InventoryContext::Preflight => false,
        #[cfg(any(test, feature = "fixture"))]
        InventoryContext::Sandboxed => true,
        InventoryContext::Launch(task, route) => {
            let minimal = provenant_confinement::permissions::MandatoryRoots::new(
                roots.home,
                roots.state,
                roots.cwd,
            )?;
            let policy = host_writes.policy_for_launch(roots.state, minimal);
            sandbox_mode_for_access(
                host_writes,
                route,
                task,
                policy
                    .optional()
                    .network_for(access.network(writer.is_some())),
                writer.is_none(),
            )
            .sandboxed
        }
    };
    let mut mandatory =
        provenant_confinement::permissions::MandatoryRoots::for_host_with_scratch_mode(
            host_roots,
            roots.home,
            roots.cwd,
            roots.scratch,
            roots.credential_roots,
            roots.github_roots,
            roots.protected_worktrees,
            sandboxed,
        )?;
    if writer.is_none() && access.source_root.is_some() && !access.source_live {
        // The host materialises a commit source beside candidate scratch under
        // state. Its ordinary read root cannot override the state denial, and
        // every adapter starts in this cwd; keep the view immutable.
        mandatory.add_state_read_exec(grant(roots.cwd))?;
    }
    for path in roots
        .prompt_file
        .into_iter()
        .chain(access.attachments.iter().map(PathBuf::as_path))
    {
        mandatory.add_state_read(PathGrant {
            path: path.to_owned(),
            scope: PathScope::Exact,
        })?;
    }
    if let Some(path) = &access.inputs_root {
        mandatory.add_state_read(grant(path))?;
    }
    // Product releases are host-verified before preparation; arbitrary assets
    // do not open another state subtree.
    if roots.assets.starts_with(roots.state.join("releases")) {
        mandatory.add_state_read_exec(grant(roots.assets))?;
    }
    if let Some(writer) = writer {
        for path in [
            writer.worktree.repo().common_git_dir(),
            writer.worktree.repo().private_git_dir(),
        ] {
            mandatory.add_git(grant(path))?;
        }
    }
    let writes = write_roots(
        access,
        host_writes,
        writer.is_some(),
        &project_id.to_string(),
    );
    // The host owner's writer roots may sit inside another checkout, such as
    // the primary checkout's ignored lock directory; dispatch roots may not.
    if writer.is_some() {
        for path in host_writes.writer_write_roots_for(&project_id.to_string()) {
            let write = grant(path);
            mandatory.add_host_write(&write)?;
            mandatory.add_host_write(&PathGrant {
                path: resolve_policy_path(path)?,
                scope: write.scope,
            })?;
        }
    }
    for write in &writes {
        mandatory.validate_extra_write(write)?;
        mandatory.validate_extra_write(&PathGrant {
            path: resolve_policy_path(&write.path)?,
            scope: write.scope,
        })?;
    }
    let cache_reads = sandbox_cache_reads(
        host_writes,
        &project_id.to_string(),
        writes
            .iter()
            .chain([grant(roots.scratch), grant(roots.private_state)].iter())
            .chain(writer.map(|writer| grant(writer.worktree.path())).iter()),
    );
    let mut policy = host_writes.policy_for_launch(roots.state, mandatory);
    policy.resolve_optional_aliases(resolve_policy_path)?;
    let network = policy
        .optional()
        .network_for(access.network(writer.is_some()));
    Ok(PlanRequest {
        scope: WorkScope::Project(project_id),
        permissions: PlanPermissions {
            policy: Some(policy),
            read_only: None,
            mode,
            scratch: grant(roots.scratch),
            private_state: Some(grant(roots.private_state)),
            read_roots: std::iter::once(grant(roots.assets))
                .chain(writer.is_none().then(|| grant(roots.cwd)))
                .chain(
                    writer
                        .and_then(|writer| writer.worktree.repo().common_git_dir().parent())
                        .into_iter()
                        .flat_map(project_scratch_reads),
                )
                .chain(access.read_roots.iter().map(|path| grant(path)))
                .chain(access.attachments.iter().map(|path| PathGrant {
                    path: path.clone(),
                    scope: PathScope::Exact,
                }))
                .chain(access.inputs_root.iter().map(|path| grant(path)))
                .chain(roots.prompt_file.map(|path| PathGrant {
                    path: path.to_owned(),
                    scope: PathScope::Exact,
                }))
                .collect(),
            protected_roots: cache_reads,
            secret_roots: Vec::new(),
            selected_authentication: AdapterSpec::get(adapter)
                .and_then(|spec| spec.authentication)
                .map(Into::into),
            include_continuation: false,
            write_roots: writes,
            network,
            nested_confinement: false,
        },
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
}

/// Resolve the lane's sandbox from its route id (`adapter/model`), the
/// sealed `lanes.sandbox` knob, the parent flag and the access the plan
/// grants. The parent flag only narrows, so the host may set it on either the
/// task or its profile. A knob value that is not a string sandboxes.
pub(super) fn sandbox_mode(
    host: &provenant_confinement::run_config::RunConfig,
    route: &str,
    task: &run::TaskRequest,
    plan: &ConfinedPlan,
) -> provenant_confinement::run_config::SandboxMode {
    let permissions = plan.permissions();
    let mode = sandbox_mode_for_access(
        host,
        route,
        task,
        permissions.network,
        matches!(permissions.mode, AccessMode::ReadOnly),
    );
    if let Some(warning) = &mode.warning {
        eprintln!("provenant: {warning}");
    }
    mode
}

fn sandbox_mode_for_access(
    host: &provenant_confinement::run_config::RunConfig,
    route: &str,
    task: &run::TaskRequest,
    network: NetworkAccess,
    read_only: bool,
) -> provenant_confinement::run_config::SandboxMode {
    use provenant_confinement::run_config::SandboxInputs;
    host.sandbox_for(SandboxInputs {
        route,
        knob: task
            .profile
            .knobs
            .get("lanes.sandbox")
            .map(|value| value.as_str().unwrap_or("on")),
        parent_sandboxed: [&task.extra, &task.profile.extra].into_iter().any(|extra| {
            extra
                .get(provenant_records::run::PARENT_SANDBOXED_FIELD)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        }),
        network_narrowed: network != NetworkAccess::Open,
        read_only,
        os_sandboxed: provenant_confinement::run_config::current_process_is_sandboxed(),
        platform_supported: cfg!(target_os = "macos"),
    })
}

/// Tell the lane its mode (`PROVENANT_SANDBOX=on|off`) and record it, with
/// its source, in the applied route that the receipt and `status` read.
pub(super) fn apply_sandbox_mode(
    mode: provenant_confinement::run_config::SandboxMode,
    environment: &mut Vec<(std::ffi::OsString, std::ffi::OsString)>,
    route: &mut provenant_records::run::RouteRecord,
) {
    use provenant_confinement::run_config::SANDBOX_ENV;
    environment.retain(|(name, _)| name != SANDBOX_ENV);
    environment.push((SANDBOX_ENV.into(), mode.env_value().into()));
    route.extra.insert(
        provenant_records::run::SANDBOX_ROUTE_FIELD.into(),
        serde_json::json!(mode),
    );
}

/// The configured host caches a sandboxed lane reads but never writes. They
/// render as protected roots: readable, with writes denied. A path that does
/// not exist, or that overlaps one of the lane's own write roots, scratch,
/// private state or worktree, is skipped so the lane keeps the grant it
/// already has there.
fn sandbox_cache_reads<'a>(
    host: &provenant_confinement::run_config::RunConfig,
    project: &str,
    own: impl Iterator<Item = &'a PathGrant> + Clone,
) -> Vec<PathGrant> {
    host.sandbox_cache_reads_for(project)
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .filter(|path| path.is_dir())
        .map(|path| PathGrant {
            path,
            scope: PathScope::Subtree,
        })
        .filter(|cache| !own.clone().any(|grant| grant.overlaps(cache)))
        .collect()
}

/// Resolve existing ancestors too: a mandatory store remains denied when the
/// leaf is absent at compile time or its public spelling traverses a symlink.
fn resolve_policy_path(
    path: &Path,
) -> Result<PathBuf, provenant_confinement::permissions::PermissionError> {
    provenant_confinement::permissions::resolve_boundary_path(path)
}

pub(super) fn protected_worktrees(
    registry: &Registry<'_>,
    source: &Path,
    git: &Path,
) -> Result<Vec<PathGrant>, String> {
    let roots = registry.roots.ok_or("routing context is unavailable")?;
    let utility_home = host_git_home(roots.state.as_path())?;
    let utility = provenant_confinement::UtilityRuntime::new(git, utility_home.path())
        .map_err(|error| error.to_string())?;
    let executor = provenant_os::exec::SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let budget = provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    let cancel = provenant_core::CancelToken::new();
    let ancestor = source.ancestors().find(|path| path.join(".git").exists());
    let selected = ancestor.unwrap_or(source);
    let mut paths = Vec::new();
    if let Some(ancestor) = ancestor {
        let repo = utility
            .open_read_repo(&executor, ancestor, budget, &cancel)
            .map_err(|error| error.to_string())?;
        paths.extend(
            registry
                .repo_facts
                .read_only_worktree_paths(&repo, &executor, &utility, budget, &cancel)
                .map_err(|error| error.to_string())?,
        );
    }
    let mut protected = Vec::new();
    for path in paths {
        let alias = resolve_policy_path(&path).map_err(|error| error.to_string())?;
        if alias == selected {
            continue;
        }
        protected.push(PathGrant {
            path: path.clone(),
            scope: PathScope::Subtree,
        });
        if alias != path {
            protected.push(PathGrant {
                path: alias,
                scope: PathScope::Subtree,
            });
        }
    }
    protected.sort_by(|left, right| left.path.cmp(&right.path));
    protected.dedup();
    Ok(protected)
}

/// Inventory names and registration metadata only; credential contents are
/// never needed to establish the mandatory boundary.
pub(super) fn credential_roots(
    registry: &Registry<'_>,
    home: &Path,
    settings: &provenant_providers::accounts::Settings,
) -> Result<Vec<PathGrant>, String> {
    let mut paths: Vec<PathBuf> = settings
        .entries
        .values()
        .flat_map(|entry| std::iter::once(&entry.directory).chain(&entry.directories))
        .filter(|path| path.as_path() != home)
        .cloned()
        .collect();
    if let Some(host) = registry.host {
        paths.extend(host.default_account_directories.values().cloned());
    }
    paths.sort();
    paths.dedup();
    let mut grants: Vec<_> = paths
        .into_iter()
        .map(|path| PathGrant {
            path,
            scope: PathScope::Subtree,
        })
        .collect();
    // Claude declares a sibling JSON profile as well as files inside its
    // account directory. Keep this exact endpoint for every registered
    // spelling, including identities this attempt did not select.
    for entry in settings.entries.values() {
        if AdapterSpec::get(&entry.adapter).is_some_and(|spec| spec.credential_profile_sibling) {
            grants.extend(
                std::iter::once(&entry.directory)
                    .chain(&entry.directories)
                    .filter(|path| path.as_path() != home)
                    .map(|path| PathGrant {
                        path: path.with_extension("json"),
                        scope: PathScope::Exact,
                    }),
            );
        }
    }
    grants.extend(
        registry
            .host
            .map(|host| host.credential_roots.clone())
            .unwrap_or_else(run::HostLaunchContext::credential_roots),
    );
    Ok(grants)
}

pub(super) fn github_roots(registry: &Registry<'_>) -> Vec<PathGrant> {
    registry
        .host
        .map(|host| host.github_roots.clone())
        .unwrap_or_else(run::HostLaunchContext::github_roots)
        .into_iter()
        .map(|path| PathGrant {
            path,
            scope: PathScope::Subtree,
        })
        .collect()
}

pub(super) fn host_provider_var(registry: &Registry<'_>, name: &str) -> Option<std::ffi::OsString> {
    if name == "SSH_AUTH_SOCK" {
        return None;
    }
    if let Some(host) = registry.host
        && matches!(
            name,
            "CODEX_HOME"
                | "CLAUDE_CONFIG_DIR"
                | "GH_CONFIG_DIR"
                | "XDG_CONFIG_HOME"
                | "XDG_DATA_HOME"
        )
    {
        return host.provider_path_environment.get(name).cloned();
    }
    std::env::var_os(name)
}

/// Host token delivery must respect the same typed GitHub exception as file
/// access; acquiring a token outside the lane cannot bypass an explicit denial.
pub(super) fn github_auth_permitted(
    plan: &ConfinedPlan,
    profile: &provenant_confinement::permissions::ProviderProfileSpec,
) -> bool {
    if plan.permissions().network != NetworkAccess::Open
        || profile.deny_credential_access
        || profile.deny_network
    {
        return false;
    }
    plan.permissions().policy.as_ref().is_none_or(|policy| {
        !policy.optional().deny_read.iter().any(|denied| {
            policy
                .mandatory()
                .github()
                .iter()
                .any(|root| denied.overlaps(root))
        })
    })
}

/// Home and TMPDIR for the host's Git utility: a fresh private directory under
/// the host root, never the run state root. Inside a lane that is the
/// lane-writable child store, where the lane could pre-create or populate it.
pub(super) fn host_git_home(
    host_root: &Path,
) -> Result<provenant_providers::host::HostProbeDir, String> {
    provenant_providers::host::HostProbeDir::create(host_root).map_err(|error| error.to_string())
}

pub(super) fn source_identity(path: &Path) -> Result<run::SourceIdentity, String> {
    let directory = Directory::open_anchor(path).map_err(|error| error.to_string())?;
    let metadata = directory.metadata().map_err(|error| error.to_string())?;
    Ok(run::SourceIdentity {
        device: metadata.device,
        inode: metadata.inode,
    })
}

/// Reopen the graph-bound checkout at launch, refusing a moved or replaced
/// worktree rather than falling back to the daemon's checkout.
#[derive(Debug, thiserror::Error)]
pub(super) enum SourceCwdError {
    #[error("{0}")]
    Authority(String),
    #[error("{0}")]
    Operational(String),
}
impl SourceCwdError {
    pub(super) fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        match self {
            Self::Authority(_) => provenant_records::diagnostic::BoundaryClass::Authority,
            Self::Operational(_) => provenant_records::diagnostic::BoundaryClass::None,
        }
    }
}

pub(super) fn source_cwd(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
) -> Result<PathBuf, SourceCwdError> {
    let cwd = registry
        .cwd
        .ok_or_else(|| SourceCwdError::Operational("working directory is unavailable".into()))?;
    let Some(source) = &task.access.source_root else {
        if task.access.source_identity.is_some() || task.access.source_live {
            return Err(SourceCwdError::Authority(
                "graph source identity has no checkout".into(),
            ));
        }
        return canonical(cwd).map_err(SourceCwdError::Operational);
    };
    if task.writer.is_some() || (!task.access.source_live && task.head.is_none()) {
        return Err(SourceCwdError::Authority(
            "read-only source needs a pinned head and no writer".into(),
        ));
    }
    let state = registry
        .state_root
        .ok_or_else(|| SourceCwdError::Operational("state root is unavailable".into()))?;
    if !run::access_root_allowed(source, state, true)
        || provenant_confinement::refuses_lane_root(source, true, registry.home)
    {
        return Err(SourceCwdError::Authority(
            "graph source checkout is not an allowed read root".into(),
        ));
    }
    let verified = if task.access.source_live {
        // A live child may select another project, just as an ordinary CLI
        // launch can. The host sealed its canonical root and filesystem identity;
        // immutable review snapshots still require project registration below.
        canonical(source).map_err(SourceCwdError::Authority)?
    } else {
        run::resolve_reader_source(state, cwd, source).map_err(|_| {
            SourceCwdError::Authority(
                "graph source checkout is no longer registered with the project".into(),
            )
        })?
    };
    if verified != *source {
        return Err(SourceCwdError::Authority(
            "graph source checkout changed location".into(),
        ));
    }
    let identity = source_identity(&verified).map_err(SourceCwdError::Authority)?;
    if task.access.source_identity.as_ref() != Some(&identity) {
        return Err(SourceCwdError::Authority(
            "graph source checkout identity changed".into(),
        ));
    }
    Ok(verified)
}

#[derive(Debug, thiserror::Error)]
pub(super) enum SourceHeadError {
    #[error("graph source head missing")]
    Missing,
    #[error("invalid graph source head")]
    Invalid,
    #[error("graph source checkout moved from pinned head {0}")]
    Moved(provenant_core::GitOid),
}

pub(super) fn source_head(
    task: &run::TaskRequest,
) -> Result<provenant_core::GitOid, SourceHeadError> {
    task.head
        .as_deref()
        .ok_or(SourceHeadError::Missing)?
        .parse()
        .map_err(|_| SourceHeadError::Invalid)
}

pub(super) fn require_source_head(
    task: &run::TaskRequest,
    observed: Option<provenant_core::GitOid>,
) -> Result<(), SourceHeadError> {
    if task.access.source_root.is_some() && !task.access.source_live {
        let expected = source_head(task)?;
        if observed != Some(expected) {
            return Err(SourceHeadError::Moved(expected));
        }
    }
    Ok(())
}

/// Verify the selected repository on the host and match the persisted dispatch
/// project before deriving any Git read authority from that binding.
pub(super) type PreparedReadPlan = (
    ConfinedPlan,
    Option<provenant_core::GitOid>,
    Option<provenant_records::AbsolutePath>,
    Option<PathBuf>,
);

#[derive(Debug, thiserror::Error)]
pub(super) enum ReadOnlyPlanError {
    #[error(transparent)]
    Run(#[from] run::RunError),
    #[error(transparent)]
    Plan(#[from] provenant_confinement::PlanError),
    #[error("repository does not match dispatch project binding")]
    BindingMismatch,
    #[error("dispatch project binding missing")]
    MissingBinding,
    #[error("pinned source requires a Git repository")]
    PinnedSourceRequiresGit,
    #[error("{0}")]
    Other(String),
}

impl From<String> for ReadOnlyPlanError {
    fn from(value: String) -> Self {
        Self::Other(value)
    }
}
impl From<&str> for ReadOnlyPlanError {
    fn from(value: &str) -> Self {
        Self::Other(value.into())
    }
}
impl ReadOnlyPlanError {
    pub(super) fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass;
        match self {
            Self::Run(error) => error.boundary_class(),
            Self::Plan(error) => error.boundary_class(),
            Self::BindingMismatch | Self::MissingBinding | Self::PinnedSourceRequiresGit => {
                BoundaryClass::Authority
            }
            Self::Other(_) => BoundaryClass::None,
        }
    }
}

#[test]
fn read_only_plan_preserves_typed_authority_failures() {
    use provenant_records::diagnostic::BoundaryClass;
    assert_eq!(
        ReadOnlyPlanError::BindingMismatch.boundary_class(),
        BoundaryClass::Authority
    );
    assert_eq!(
        ReadOnlyPlanError::MissingBinding.boundary_class(),
        BoundaryClass::Authority
    );
    assert_eq!(
        ReadOnlyPlanError::PinnedSourceRequiresGit.boundary_class(),
        BoundaryClass::Authority
    );
    assert_eq!(
        ReadOnlyPlanError::Other("unavailable".into()).boundary_class(),
        BoundaryClass::None
    );
    assert_eq!(
        ReadOnlyPlanError::Run(run::RunError::WriterHeadNotLaneBranch).boundary_class(),
        BoundaryClass::LaneGit
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_only_plan(
    roots: &provenant_core::Roots,
    project: ProjectId,
    cwd: &Path,
    view: &Path,
    git: &Path,
    mut request: PlanRequest,
    isolated: bool,
    live: bool,
) -> Result<PreparedReadPlan, ReadOnlyPlanError> {
    use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime, run_read_utility};
    use provenant_os::exec::SystemExecutor;

    // The host-pinned Git, the same one the lane receives as PROVENANT_GIT.
    let utility_home = host_git_home(roots.state.as_path())?;
    let utility =
        UtilityRuntime::new(git, utility_home.path()).map_err(|error| error.to_string())?;
    let executor = SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let budget = UtilityBudget::bounded(std::time::Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    let cancel = provenant_core::CancelToken::new();
    // Find the root without opening a protected-history repo: read-only
    // repositories may use native object alternates. A malformed Git marker
    // must refuse, never fall through to a non-Git workspace.
    let mut repository = None;
    for ancestor in cwd.ancestors() {
        let directory = Directory::open_anchor(ancestor).map_err(|error| error.to_string())?;
        match directory.metadata_at(std::ffi::OsStr::new(".git"), false) {
            Ok(_) => {
                repository = Some(
                    utility
                        .open_read_repo(&executor, ancestor, budget, &cancel)
                        .map_err(|error| error.to_string())?,
                );
                break;
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.to_string().into()),
        }
    }
    let location = provenant_config::ProjectLocation::new(
        repository
            .as_ref()
            .map_or(cwd, |repo| repo.common_git_dir()),
    )
    .map_err(|error| error.to_string())?;
    let binding = if live {
        // Graph attribution stays with the parent; the read plan belongs to
        // the selected checkout, as it does for an ordinary host launch.
        let binding =
            provenant_config::bind_project(roots, location).map_err(|error| error.to_string())?;
        request.scope = WorkScope::Project(binding.project_id());
        binding
    } else {
        let binding = provenant_config::lookup_project(roots, &location)
            .map_err(|error| error.to_string())?
            .ok_or(ReadOnlyPlanError::MissingBinding)?;
        if binding.project_id() != project {
            return Err(ReadOnlyPlanError::BindingMismatch);
        }
        binding
    };
    match repository {
        Some(repo) => {
            if let Some(policy) = request.permissions.policy.as_mut() {
                // A launch from a checkout subdirectory still protects the
                // entire verified source; an isolated view also protects its
                // original live checkout.
                policy
                    .mandatory_mut()
                    .add_source_alias(PathGrant {
                        path: repo.root_path().to_owned(),
                        scope: PathScope::Subtree,
                    })
                    .map_err(|error| error.to_string())?;
                for path in [repo.common_git_dir(), repo.private_git_dir()] {
                    policy
                        .mandatory_mut()
                        .add_git(PathGrant {
                            path: path.to_owned(),
                            scope: PathScope::Subtree,
                        })
                        .map_err(|error| error.to_string())?;
                }
                for root in &request.permissions.write_roots {
                    policy
                        .mandatory()
                        .validate_extra_write(root)
                        .map_err(|error| error.to_string())?;
                }
            }
            // The host's own read of the checkout HEAD is the only reviewed
            // head a read-only lane can claim; an unreadable HEAD stays unproven.
            let head = run_read_utility(
                &executor,
                GitRead::RevParseHead,
                &repo,
                &utility,
                budget,
                &cancel,
            )
            .ok()
            .and_then(|out| String::from_utf8(out.stdout.bytes).ok())
            .and_then(|text| provenant_core::GitOid::parse(text.trim()).ok());
            let scratch = request.permissions.scratch.path.clone();
            let plan = if live {
                run::live_read_only_plan(&repo, &binding, request)
            } else if isolated {
                run::isolated_read_only_plan(&roots.state, &repo, &binding, request)
            } else {
                run::read_only_plan(&roots.state, &repo, &binding, request)
            };
            let plan = plan.map_err(ReadOnlyPlanError::from)?;
            let view_git = isolated.then(|| view.join(".git"));
            let view_head = if let Some(path) = &view_git {
                let raw = std::fs::read_to_string(path.join("HEAD"))
                    .map_err(|error| error.to_string())?;
                Some(provenant_core::GitOid::parse(raw.trim()).map_err(|error| error.to_string())?)
            } else {
                head
            };
            run::prepare_read_only_git(git, &repo, &scratch, view_git.as_deref(), view_head)
                .map_err(ReadOnlyPlanError::from)?;
            Ok((
                plan,
                head,
                provenant_records::AbsolutePath::new(repo.common_git_dir()).ok(),
                Some(repo.private_git_dir().to_owned()),
            ))
        }
        // Non-Git workspaces have no Git metadata or repository exceptions.
        None if isolated => Err(ReadOnlyPlanError::PinnedSourceRequiresGit),
        None => ConfinedPlan::new(request)
            .map(|plan| (plan, None, None, None))
            .map_err(ReadOnlyPlanError::from),
    }
}

/// Advisory freshness of the checkout a read-only lane reads, for the launch
/// warnings. The lane reads `source`: the `--worktree` checkout, or the caller's
/// checkout when none is named. Absent or unusable Git data yields no warning.
pub(in crate::run) fn read_only_checkout_warning(state: &Path, source: &Path) -> Option<String> {
    use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime, run_read_utility};
    use provenant_os::exec::SystemExecutor;

    let git = provenant_os::git::git_executable()?;
    let utility_home = host_git_home(state).ok()?;
    let utility = UtilityRuntime::new(&git, utility_home.path()).ok()?;
    let executor = SystemExecutor::new(1).ok()?;
    let budget = UtilityBudget::bounded(std::time::Duration::from_secs(5)).ok()?;
    let cancel = provenant_core::CancelToken::new();
    let ancestor = source.ancestors().find(|path| path.join(".git").exists())?;
    let repo = utility
        .open_read_repo(&executor, ancestor, budget, &cancel)
        .ok()?;
    let head = run_read_utility(
        &executor,
        GitRead::RevParseHead,
        &repo,
        &utility,
        budget,
        &cancel,
    )
    .ok()
    .and_then(|out| String::from_utf8(out.stdout.bytes).ok())
    .and_then(|text| provenant_core::GitOid::parse(text.trim()).ok())?;
    let count = commits_behind_default(&executor, &repo, &utility, budget, &cancel, head)?;
    Some(format!(
        "{} is {count} commit{} behind origin's default branch; read-only lane will read {head}",
        source.display(),
        if count == 1 { "" } else { "s" }
    ))
}

/// Compare `head` with the remote's default branch as last fetched. This never
/// fetches. The remote is asked only when no `origin/HEAD` is recorded, and only
/// within the utility budget.
fn commits_behind_default(
    executor: &dyn provenant_os::exec::Executor,
    repo: &provenant_confinement::ReadOnlyRepo,
    utility: &provenant_confinement::UtilityRuntime,
    budget: provenant_confinement::UtilityBudget,
    cancel: &provenant_core::CancelToken,
    head: provenant_core::GitOid,
) -> Option<u64> {
    use provenant_confinement::{GitRead, run_read_utility, utility::advertised_default_branch};
    use provenant_os::exec::Termination;

    let resolve = |reference: &str| {
        let out = run_read_utility(
            executor,
            GitRead::ResolveRevision {
                revision: provenant_confinement::repo::GitRevision::new(reference).ok()?,
            },
            repo,
            utility,
            budget,
            cancel,
        )
        .ok()?;
        if out.termination != Termination::Exit(0) {
            return None;
        }
        provenant_core::GitOid::parse(std::str::from_utf8(&out.stdout.bytes).ok()?.trim()).ok()
    };
    let default = resolve("refs/remotes/origin/HEAD").or_else(|| {
        let branch =
            advertised_default_branch(executor, repo, utility, "origin", budget, cancel).ok()??;
        resolve(&format!("refs/remotes/origin/{}", branch.short_name()))
    })?;
    let behind = run_read_utility(
        executor,
        GitRead::AncestorCount {
            base: head,
            head: default,
        },
        repo,
        utility,
        budget,
        cancel,
    )
    .ok()?;
    if behind.termination != Termination::Exit(0) {
        return None;
    }
    std::str::from_utf8(&behind.stdout.bytes)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|count| *count > 0)
}

/// Publishing hands the lane the GitHub credential. It is on by default for
/// every lane on open network, read-only lanes included; `lanes.publish=false`
/// withholds it. A missing or
/// non-boolean value takes the registry default.
pub(super) fn publishing_requested(
    open_network: bool,
    knobs: &std::collections::BTreeMap<String, serde_json::Value>,
) -> bool {
    open_network
        && knobs
            .get("lanes.publish")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
}

pub(super) fn publishing_executable(
    home: &Path,
    locate: &impl Fn(&str) -> Result<PathBuf, String>,
    host_token: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if !host_token.is_some_and(super::gh_auth::valid_host_token)
        && !home.join(".config/gh").is_dir()
    {
        return None;
    }
    // Resolve links before adding exact executable grants or writing wrappers.
    let executable = canonical(&locate("gh").ok()?).ok()?;
    (executable.is_file() && provenant_os::sys::is_executable(&executable)).then_some(executable)
}

pub(crate) fn resolve_executable(name: &str) -> Result<PathBuf, String> {
    canonical(&find_executable(name)?)
}

pub(super) fn host_executable(
    host: &run::HostLaunchContext,
    name: &str,
) -> Result<PathBuf, String> {
    // The caller holds this provider's lease. Retry only an omitted entry,
    // using host-sealed directories rather than the guardian's environment.
    let path = match host.executables.get(name) {
        Some(path) => path.clone(),
        None => find_executable_on_path(name, &host.executable_search_path)?,
    };
    canonical(&path)
}

pub(crate) fn find_executable(name: &str) -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").ok_or("PATH is unavailable")?;
    find_executable_on_path(name, &std::env::split_paths(&path).collect::<Vec<_>>())
}

pub(super) fn find_executable_on_path(
    name: &str,
    search_path: &[PathBuf],
) -> Result<PathBuf, String> {
    let name = AdapterSpec::get(name).map_or(name, |spec| spec.binary);
    find_command_on_path(name, search_path)
}

pub(super) fn find_command_on_path(name: &str, search_path: &[PathBuf]) -> Result<PathBuf, String> {
    for directory in search_path
        .iter()
        .filter(|directory| directory.is_absolute())
    {
        let candidate = directory.join(name);
        if candidate.is_file() && provenant_os::sys::is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!("{name} executable was not found on PATH"))
}

pub(super) fn canonical(path: &Path) -> Result<PathBuf, String> {
    provenant_os::fs::canonicalize(path).map_err(|error| error.to_string())
}

/// Generated files stay in scratch and links in scratch or private state.
/// Directories may use any run-owned root, including trusted shared state, and
/// the selected Claude account's `projects/` tree. Traversal never follows links.
pub(super) fn apply_setup(
    steps: &[SetupStep],
    roots: &[&Path],
    claude_account: Option<&Path>,
) -> Result<(), String> {
    for step in steps {
        match step {
            SetupStep::CreateDir(path) => {
                let transcript_root =
                    claude_account.filter(|directory| path.starts_with(directory.join("projects")));
                match transcript_root {
                    Some(directory) => apply_setup_directory(&[directory], path)?,
                    None => apply_setup_directory(roots, path)?,
                };
            }
            SetupStep::WriteFile { path, bytes } => write_setup_file(roots, path, bytes, false)?,
            SetupStep::WriteExecutable { path, bytes } => {
                write_setup_file(roots, path, bytes, true)?;
            }
            SetupStep::Symlink { link, target } => {
                let (parent, name) = setup_parent(&roots[..roots.len().min(2)], link, false)?;
                let matches = match parent.metadata_at(name, false) {
                    Ok(metadata) if metadata.kind == FileKind::Symlink => {
                        let matches =
                            parent.read_link(name).map_err(|error| error.to_string())? == *target;
                        if matches {
                            parent.sync().map_err(|error| error.to_string())?;
                        }
                        matches
                    }
                    Ok(_) => false,
                    Err(error) if error.is_not_found() => false,
                    Err(error) => return Err(error.to_string()),
                };
                if !matches {
                    parent
                        .replace_link(name, target)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn write_setup_file(
    roots: &[&Path],
    path: &Path,
    bytes: &[u8],
    executable: bool,
) -> Result<(), String> {
    let (parent, name) = setup_parent(&roots[..1], path, false)?;
    let matches = match parent.metadata_at(name, false) {
        Ok(metadata) if metadata.kind == FileKind::Regular => {
            let mut file = parent.open_read(name).map_err(|error| error.to_string())?;
            match file.read_bounded(bytes.len() as u64) {
                Ok(existing) if existing == *bytes => {
                    file.sync().map_err(|error| error.to_string())?;
                    parent.sync().map_err(|error| error.to_string())?;
                    true
                }
                Ok(_) | Err(FsError::TooLarge { .. }) => false,
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(_) => false,
        Err(error) if error.is_not_found() => false,
        Err(error) => return Err(error.to_string()),
    };
    if !matches {
        parent
            .atomic_write(name, ".provider-setup.", bytes, PublishMode::Replace)
            .map_err(|error| error.to_string())?;
    }
    if executable {
        parent
            .open_read(name)
            .and_then(|file| file.make_owner_executable())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(super) fn apply_setup_directory(roots: &[&Path], path: &Path) -> Result<Directory, String> {
    let (parent, name) = setup_parent(roots, path, true)?;
    setup_directory(&parent, name)
}

pub(super) fn setup_parent<'a>(
    roots: &[&Path],
    path: &'a Path,
    create: bool,
) -> Result<(Directory, &'a std::ffi::OsStr), String> {
    let (root, relative) = roots
        .iter()
        .find_map(|root| Some((root, path.strip_prefix(root).ok()?)))
        .ok_or("provider setup path is outside run-owned state")?;
    let mut names = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(name) => names.push(name),
            _ => return Err("provider setup path is not normalised".into()),
        }
    }
    let name = names.pop().ok_or("provider setup path names its root")?;
    let mut directory = Directory::open_anchor(root).map_err(|error| error.to_string())?;
    for component in names {
        directory = if create {
            setup_directory(&directory, component)?
        } else {
            directory
                .open_verified_directory(component)
                .map_err(|error| error.to_string())?
        };
    }
    Ok((directory, name))
}

// Existing shared ancestors need only read access in a confined MCP.
pub(super) fn setup_directory(
    parent: &Directory,
    name: &std::ffi::OsStr,
) -> Result<Directory, String> {
    match parent.open_verified_directory(name) {
        Ok(directory) => Ok(directory),
        Err(error) if error.is_not_found() => {
            match parent.mkdir(name, 0o700) {
                Ok(()) => parent.sync().map_err(|error| error.to_string())?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.to_string()),
            }
            parent
                .open_verified_directory(name)
                .map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Verify the pinned writer source and describe retained predecessor edits.
/// A continuation keeps dirty work in place; unreadable status and mismatched
/// source evidence still refuse preparation.
pub(super) fn writer_source_context(
    writer: &run::WriterLaunch,
    task: &run::TaskRequest,
    git: &Path,
) -> Result<Option<String>, PrepareError> {
    let Some(source_head) = task.source_head.as_deref() else {
        if let Some(head) = task.head.as_deref() {
            writer.require_source(head.parse().map_err(|_| "invalid pinned writer source")?)?;
        }
        return Ok(None);
    };
    let uncommitted = writer
        .uncommitted_paths(git)
        .map_err(|error| format!("cannot inspect uncommitted predecessor edits: {error}"))?;
    let predecessor = task
        .head
        .as_deref()
        .ok_or("missing predecessor head for writer continuation")?
        .parse()
        .map_err(|_| "invalid pinned writer predecessor head")?;
    let source = source_head
        .parse()
        .map_err(|_| "invalid pinned writer source")?;
    if let Err(error) = writer.require_continuation_source(git, source, predecessor) {
        return Err(PrepareError::WriterContinuation(
            if uncommitted.is_empty() {
                error.to_string()
            } else {
                format!(
                    "{error}; uncommitted predecessor edits: {}",
                    uncommitted.join(", ")
                )
            },
        ));
    }
    Ok((!uncommitted.is_empty()).then(|| {
        format!(
            "Uncommitted predecessor edits retained: {}. Inspect these files and continue the work without discarding them.",
            uncommitted.join(", ")
        )
    }))
}

#[cfg(test)]
mod alias_tests {
    use super::*;

    #[test]
    fn only_the_temporary_variables_move_to_the_alias() {
        let scratch = Path::new("/state/runs/r/attempts/a/tmp/candidate-0");
        let alias = Path::new("/private/tmp/pv-0123/candidate-0");
        let os = std::ffi::OsString::from;
        let mut env = vec![
            (os("TMPDIR"), scratch.into()),
            (os("TMPPREFIX"), scratch.join("zsh").into()),
            (os("CARGO_TARGET_DIR"), scratch.join("target").into()),
            (os("TMPDIR_OTHER"), scratch.into()),
        ];
        alias_temporary_env(&mut env, scratch, alias);
        assert_eq!(env[0].1, alias.as_os_str());
        assert_eq!(env[1].1, alias.join("zsh").as_os_str());
        assert_eq!(env[2].1, scratch.join("target").as_os_str());
        assert_eq!(env[3].1, scratch.as_os_str());
    }
}
