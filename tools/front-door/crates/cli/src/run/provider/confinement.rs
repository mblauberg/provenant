use super::*;
use provenant_records::warning::{Warning, warn};

/// Bind declared lane capabilities to the provider's confinement profile.
pub(in crate::run) fn apply_lane_capabilities(
    profile: &mut provenant_confinement::permissions::ProviderProfileSpec,
    access: &run::LaneAccessRequest,
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
    pub(super) credential_roots: &'a provenant_core::CredentialRoots,
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

/// Point the lane's temporary-directory variables at the short alias of its
/// scratch directory under `attempt-scratch/<id>/tmp`, leaving every other
/// path, and every grant, on the real one. The alias target is outside the
/// attempt record directory, so a relative write from `TMPDIR` cannot reach
/// settlement's files (#3947).
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
        "run.sqlite3",
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

/// Read access to the primary checkout's project scratch (`.provenant/scratch`, or
/// `.agent-run`), where briefs and graph documents live. Git excludes it, so a
/// writer's linked worktree does not hold it. Final-component links are refused;
/// parent links may only resolve within the canonical primary checkout.
pub(super) fn project_scratch_reads(project: &Path) -> Vec<PathGrant> {
    let Ok(project) = std::fs::canonicalize(project) else {
        return Vec::new();
    };
    [provenant_config::repository_files::SCRATCH, ".agent-run"]
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
    access: &run::LaneAccessRequest,
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
    Launch(
        &'a run::TaskRequest,
        &'a str,
        &'a provenant_records::LaneAccessReceipt,
    ),
    #[cfg(any(test, feature = "fixture"))]
    Sandboxed,
}

// Composition edge keeps launch authority and inventory mode explicit.
#[allow(clippy::too_many_arguments)]
pub(super) fn confinement_request(
    project_id: ProjectId,
    roots: ConfinementRoots<'_>,
    writer: Option<&run::WriterLaunch>,
    access: &run::LaneAccessRequest,
    host_writes: &provenant_confinement::run_config::RunConfig,
    adapter: &str,
    publishing: bool,
    sandbox_context: InventoryContext<'_>,
) -> Result<PlanRequest, provenant_confinement::permissions::PermissionError> {
    let lane_access = match &sandbox_context {
        InventoryContext::Launch(_, _, receipt) => (*receipt).clone(),
        InventoryContext::Preflight => provenant_records::LaneAccessReceipt {
            access: provenant_records::LaneAccess::Host,
            source: provenant_records::Layer::Default,
            key: "lanes.access".into(),
            extra: Default::default(),
        },
        #[cfg(any(test, feature = "fixture"))]
        InventoryContext::Sandboxed => provenant_records::LaneAccessReceipt {
            access: provenant_records::LaneAccess::Isolated,
            source: provenant_records::Layer::Default,
            key: "lanes.access".into(),
            extra: Default::default(),
        },
    };
    let mode = match writer {
        Some(writer) => {
            let mut authority = WorktreeWriteAuthority::for_worktree(
                &writer.worktree,
                &writer.branch,
                roots.home,
                roots.credential_roots,
                &lane_access,
            )?;
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
            fixture_roots = provenant_os::fs::bind_roots(&inputs).map_err(|_| {
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
        InventoryContext::Launch(task, route, _) => {
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
                policy.optional(),
            )
            .effective
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
            lane_access,
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
            credential_roots: policy.mandatory().credential_roots().clone(),
            policy: Some(policy),
            read_only: None,
            mode,
            scratch: grant(roots.scratch),
            private_state: Some(grant(roots.private_state)),
            read_roots: std::iter::once(grant(roots.assets))
                .chain(writer.is_none().then(|| grant(roots.cwd)))
                .chain(
                    writer
                        .into_iter()
                        .flat_map(|writer| writer.project_root().into_iter())
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
/// task or its profile. Invalid optional knobs retain the default off.
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
        permissions
            .policy
            .as_ref()
            .map(|p| p.optional())
            .unwrap_or(&Default::default()),
    );
    if let Some(message) = &mode.warning {
        warn(Warning::new(
            provenant_records::warning::SANDBOX_MODE_OVERRIDDEN,
            message.clone(),
        ));
    }
    mode
}

fn sandbox_mode_for_access(
    host: &provenant_confinement::run_config::RunConfig,
    route: &str,
    task: &run::TaskRequest,
    network: NetworkAccess,
    policy: &provenant_confinement::permissions::OptionalPolicy,
) -> provenant_confinement::run_config::SandboxMode {
    use provenant_confinement::run_config::{SandboxInputs, SandboxRestriction};
    let sandbox = task
        .profile
        .knobs
        .get("lanes.sandbox")
        .and_then(serde_json::Value::as_str);
    let strength = task
        .profile
        .knobs
        .get("lanes.sandbox_floor")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("warn");
    let parent = task.profile.parent_sandboxed;
    let (floor, warn) = provenant_records::run::admission_floor(parent, sandbox, strength);
    let restrictions = SandboxRestriction::for_policy(network, policy);
    let mut mode = host.sandbox_for(SandboxInputs {
        route,
        knob: if floor { Some("on") } else { sandbox },
        parent_sandboxed: floor,
        restrictions: &restrictions,
        os_sandboxed: provenant_confinement::run_config::current_process_is_sandboxed(),
        platform_supported: cfg!(target_os = "macos"),
    });
    if floor && mode.effective {
        mode.requested = sandbox == Some("on");
        mode.source = provenant_records::Layer::Parent;
        // The floor replaces the earlier cause outright, as it did when the
        // mode carried a single source and no separate cap.
        mode.constraint = None;
    }
    if warn {
        let source = task
            .profile
            .knob_sources
            .get("lanes.sandbox_floor")
            .copied()
            .map(provenant_records::Layer::as_str)
            .unwrap_or("default");
        let warning = format!(
            "sandbox admission floor lowered; lanes.sandbox_floor=warn (source: {source}, boundary: none)"
        );
        mode.warning = Some(match mode.warning.take() {
            Some(other) => format!("{other}; {warning}"),
            None => warning,
        });
    }
    mode
}

// Small: sealed public task/receipt round trips across every adapter candidate.
#[cfg(test)]
#[test]
fn admission_policy_is_independent_of_primary_or_fallback_adapter()
-> Result<(), Box<dyn std::error::Error>> {
    let prepared = crate::run::tests::trace_run(
        1,
        provenant_core::AttemptId::from_bytes([9; 16]),
        provenant_core::WallTime::from_micros(1)?,
    )?;
    for strength in ["off", "warn", "strict"] {
        let mut task = prepared.request.task.clone();
        task.profile.parent_sandboxed = true;
        task.profile
            .knobs
            .insert("lanes.sandbox".into(), "off".into());
        task.profile
            .knobs
            .insert("lanes.sandbox_floor".into(), strength.into());
        let task: run::TaskRequest = serde_json::from_value(serde_json::to_value(&task)?)?;
        for route in [
            "codex/gpt-6-luna",
            "claude/claude-sonnet-5-5",
            "command/fmt",
        ] {
            let mode = sandbox_mode_for_access(
                &Default::default(),
                route,
                &task,
                NetworkAccess::Open,
                &Default::default(),
            );
            if !provenant_confinement::run_config::current_process_is_sandboxed() {
                assert_eq!(mode.effective, strength == "strict");
                assert_eq!(
                    mode.warning.is_some(),
                    strength == "warn" || (strength == "strict" && !cfg!(target_os = "macos"))
                );
            } else {
                assert!(mode.effective);
            }
            let mut receipt = run::RouteRecord::default();
            let mut environment = Vec::new();
            apply_sandbox_mode(&task, mode, &mut environment, &mut receipt);
            let admission = receipt
                .sandbox_admission
                .as_ref()
                .expect("admission receipt");
            assert_eq!(
                admission.strength,
                match strength {
                    "off" => run::SandboxFloorStrength::Off,
                    "warn" => run::SandboxFloorStrength::Warn,
                    "strict" => run::SandboxFloorStrength::Strict,
                    _ => unreachable!(),
                }
            );
            assert_eq!(
                admission.boundary,
                provenant_records::diagnostic::BoundaryClass::None
            );
            assert_eq!(admission.source, provenant_records::Layer::Default);
        }
    }
    Ok(())
}

/// Tell the lane its mode (`PROVENANT_SANDBOX=on|off`) and record it, with
/// its source, in the applied route that the receipt and `status` read.
pub(super) fn apply_sandbox_mode(
    task: &run::TaskRequest,
    mode: provenant_confinement::run_config::SandboxMode,
    environment: &mut Vec<(std::ffi::OsString, std::ffi::OsString)>,
    route: &mut provenant_records::run::RouteRecord,
) {
    use provenant_core::consumer_env::SANDBOX;
    let parent = task.profile.parent_sandboxed;
    let strength = match task
        .profile
        .knobs
        .get("lanes.sandbox_floor")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("warn")
    {
        "off" => run::SandboxFloorStrength::Off,
        "strict" => run::SandboxFloorStrength::Strict,
        _ => run::SandboxFloorStrength::Warn,
    };
    route.sandbox_admission = Some(run::SandboxAdmissionReceipt {
        parent_sandboxed: parent,
        boundary: provenant_records::diagnostic::BoundaryClass::None,
        strength,
        knob: "lanes.sandbox_floor".into(),
        source: task
            .profile
            .knob_sources
            .get("lanes.sandbox_floor")
            .copied()
            .unwrap_or(provenant_records::Layer::Default),
        extra: Default::default(),
    });
    environment.retain(|(name, _)| name != SANDBOX);
    environment.push((SANDBOX.into(), mode.env_value().into()));
    // A host-layer mode with no cap is the explicit knob: the preparation edge
    // knows the knob's own layer, which the supervisor rebind does not.
    let source = if mode.source == provenant_records::Layer::Host && mode.constraint.is_none() {
        task.profile
            .knob_sources
            .get("lanes.sandbox")
            .copied()
            .unwrap_or(provenant_records::Layer::Default)
    } else {
        mode.source
    };
    route.sandbox = Some(run::SandboxModeReceipt {
        requested: mode.requested,
        effective: mode.effective,
        source,
        constraint: mode.constraint,
        warning: mode.warning,
        extra: Default::default(),
    });
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

/// Protect other projects' worktrees from a lane. A binding whose common
/// directory is gone has nothing to protect and is skipped. When a present
/// project's layout cannot be read, its whole root is protected instead of
/// refusing the launch, and the reason is returned as a preparation warning.
pub(super) fn protected_worktrees(
    registry: &Registry<'_>,
    source: &Path,
    git: &Path,
) -> Result<(Vec<PathGrant>, Vec<String>), String> {
    let roots = registry.roots.ok_or("routing context is unavailable")?;
    let utility_home = host_git_home(roots.state.as_path())?;
    let utility =
        provenant_confinement::UtilityRuntime::new(git, utility_home.path(), roots.state.as_path())
            .map_err(|error| error.to_string())?;
    let executor = provenant_os::exec::SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let budget = provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    let cancel = provenant_core::CancelToken::new();
    let ancestor = source.ancestors().find(|path| path.join(".git").exists());
    let selected = ancestor.unwrap_or(source);
    let mut paths = Vec::new();
    let mut warnings = Vec::new();
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
    for binding in provenant_config::registered_projects(roots)
        .map_err(|error| format!("cannot read registered project inventory: {error}"))?
    {
        let common = binding.common_dir();
        if !binding.common_dir_present() {
            continue;
        }
        let is_git = common.file_name() == Some(std::ffi::OsStr::new(".git"))
            || (common.join("HEAD").is_file() && common.join("objects").is_dir());
        if !is_git {
            continue;
        }
        let layout = match utility.project_layout_for_binding(&executor, &binding, budget, &cancel)
        {
            Ok(layout) => layout,
            Err(error) => {
                // Do not refuse an unrelated launch for another project's
                // unreadable layout. Protect its whole root so its worktrees
                // stay out of reach, and name the degradation.
                let root = binding
                    .primary_checkout_hint()
                    .filter(|path| path.is_dir())
                    .map(Path::to_path_buf)
                    .or_else(|| common.parent().map(Path::to_path_buf));
                match root {
                    Some(root)
                        if !selected.starts_with(&root)
                            && !root.starts_with(selected)
                            && !roots.state.as_path().starts_with(&root) =>
                    {
                        warnings.push(format!(
                            "registered project {} at {} could not be verified ({error}); protecting {}",
                            binding.project_id(),
                            common.display(),
                            root.display()
                        ));
                        paths.push(root);
                    }
                    Some(root) => warnings.push(format!(
                        "registered project {} at {} could not be verified ({error}); its root {} overlaps the launching checkout or host state, so no grant was added",
                        binding.project_id(),
                        common.display(),
                        root.display()
                    )),
                    None => warnings.push(format!(
                        "registered project {} at {} could not be verified ({error}); no project root could be protected",
                        binding.project_id(),
                        common.display()
                    )),
                }
                continue;
            }
        };
        let ports = provenant_config::PortTable::read_for(layout.project(), roots.state.as_path())
            .map_err(|error| {
                format!(
                    "cannot read worktree allocation settings for {}: {error}",
                    common.display()
                )
            })?;
        paths.extend(ports.worktree_roots_under(layout.root()));
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
    Ok((protected, warnings))
}

fn credential_declarations(
    registry: &Registry<'_>,
    home: &Path,
    settings: &provenant_providers::accounts::Settings,
) -> Vec<provenant_core::CredentialDeclaration> {
    use provenant_core::{
        CredentialDeclaration, CredentialInventory, CredentialKind, CredentialRoot, CredentialScope,
    };
    let mut declarations = CredentialDeclaration::for_home(home);

    for entry in settings.entries.values() {
        for directory in crate::accounts::credential_directories(entry) {
            let source = format!(
                "account registry: {}/{} credential store",
                entry.adapter, entry.label
            );
            if let Some(install) = AdapterSpec::get(&entry.adapter)
                .and_then(|spec| spec.install)
                .filter(|install| install.lane_home.0 == "HOME")
            {
                // A HOME-rooted account owns client state and credential files,
                // not the whole HOME. Keep nested account homes independent.
                declarations.push(CredentialDeclaration {
                    root: CredentialRoot {
                        path: directory.join(install.home),
                        source: source.clone(),
                        kind: CredentialKind::Store,
                        scope: CredentialScope::Subtree,
                    },
                    inventory: CredentialInventory::Unknown,
                });
                if let Some(spec) = AdapterSpec::get(&entry.adapter) {
                    for relative in spec
                        .credential_files
                        .iter()
                        .chain(spec.credential_auxiliary_files)
                    {
                        declarations.push(CredentialDeclaration {
                            root: CredentialRoot {
                                path: directory.join(relative),
                                source: source.clone(),
                                kind: CredentialKind::Store,
                                scope: CredentialScope::Exact,
                            },
                            inventory: CredentialInventory::Unknown,
                        });
                    }
                }
                continue;
            }
            declarations.push(CredentialDeclaration {
                root: CredentialRoot {
                    path: directory.to_path_buf(),
                    source,
                    kind: CredentialKind::Store,
                    scope: CredentialScope::Subtree,
                },
                inventory: CredentialInventory::Adapter(entry.adapter.clone()),
            });
        }
    }

    let default_directories = registry
        .host
        .map(|host| host.default_account_directories.clone())
        .unwrap_or_else(run::HostLaunchContext::default_account_directories);
    for spec in ADAPTER_SPECS {
        let Some((environment, fallback)) = spec.default_account_directory else {
            continue;
        };
        let host_override = default_directories
            .get(spec.name)
            .cloned()
            .or_else(|| host_provider_var(registry, environment).map(PathBuf::from));
        let (path, source, kind) = if let Some(path) = host_override {
            (
                path,
                format!("provider environment: {environment}"),
                CredentialKind::Store,
            )
        } else {
            (
                home.join(fallback),
                format!(
                    "provider declaration: {} default account directory",
                    spec.name
                ),
                (*spec.default_account_root_kind).clone(),
            )
        };
        if spec.credential_profile_sibling {
            declarations.push(CredentialDeclaration {
                root: CredentialRoot {
                    path: path.with_extension("json"),
                    source: format!("{source} credential file"),
                    kind: CredentialKind::Store,
                    scope: CredentialScope::Exact,
                },
                inventory: CredentialInventory::Adapter(spec.name.to_owned()),
            });
        }
        declarations.push(CredentialDeclaration {
            root: CredentialRoot {
                path,
                source,
                kind,
                scope: CredentialScope::Subtree,
            },
            inventory: CredentialInventory::Adapter(spec.name.to_owned()),
        });
    }

    declarations.extend(
        registry
            .host
            .map(|host| host.credential_roots.clone())
            .unwrap_or_else(run::HostLaunchContext::credential_roots),
    );

    // Sibling profiles are independent exact declarations, not holes in the
    // directory boundary. Inventoried endpoints also enter the canonical table.
    let siblings: Vec<_> = declarations
        .iter()
        .filter_map(|declaration| {
            let CredentialInventory::Adapter(adapter) = &declaration.inventory else {
                return None;
            };
            let spec = provenant_records::AdapterSpec::get(adapter)?;
            spec.credential_profile_sibling
                .then(|| CredentialDeclaration {
                    root: CredentialRoot {
                        path: declaration.root.path.with_extension("json"),
                        scope: CredentialScope::Exact,
                        source: declaration.root.source.clone(),
                        kind: CredentialKind::Store,
                    },
                    inventory: CredentialInventory::Unknown,
                })
        })
        .collect();
    declarations.extend(siblings);

    // Keep distinct kinds and inventories, even when path and source match.
    let mut unique = Vec::new();
    for declaration in declarations {
        if !unique.contains(&declaration) {
            unique.push(declaration);
        }
    }
    unique
}

/// Inventory names and registration metadata only; credential contents are
/// never needed to establish the mandatory boundary.
pub(super) fn credential_roots(
    registry: &Registry<'_>,
    home: &Path,
    settings: &provenant_providers::accounts::Settings,
    lane_roots: &[&Path],
    access: Option<&provenant_records::LaneAccessReceipt>,
    policy: &provenant_records::policy::Policy,
) -> Result<provenant_core::CredentialRoots, String> {
    use provenant_core::{CredentialInventory, CredentialRoots, CredentialScope};
    let declarations = credential_declarations(registry, home, settings);
    let isolated = access.is_some_and(|receipt| receipt.access.is_isolated());
    let warn = |detail: String| {
        if let Some(receipt) = access {
            let detail = format!(
                "{detail} (lanes.access={}, source: {})",
                receipt.access.as_str(),
                receipt.source.as_str()
            );
            if let provenant_records::policy::Effect::Warn(warning) = policy.check(
                "CREDENTIAL_ROOTS_OVERLAP_NONE",
                provenant_records::policy::Facts::violation(detail),
            ) {
                warn(warning);
            }
        }
    };
    let resolve = |path: &Path| {
        provenant_confinement::permissions::resolve_boundary_path(path)
            .map_err(|error| error.to_string())
    };
    let resolve_declared = |path: &Path, source: &str| {
        resolve(path).or_else(|error| {
            let detail = format!(
                "{source} at {} cannot be resolved precisely: {error}",
                path.display()
            );
            if isolated {
                Err(detail)
            } else {
                warn(detail);
                Ok(path.to_owned())
            }
        })
    };
    for decl in &declarations {
        resolve_declared(&decl.root.path, &decl.root.source)?;
    }
    let resolved_lanes = lane_roots
        .iter()
        .map(|lane| {
            let source = declarations
                .iter()
                .find(|decl| decl.root.path == **lane)
                .map_or("lane root", |decl| decl.root.source.as_str());
            resolve_declared(lane, source)
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Compare both public and resolved spellings, including aliased lane roots.
    let mut lanes = resolved_lanes;
    lanes.extend(lane_roots.iter().map(|path| path.to_path_buf()));
    lanes.sort();
    lanes.dedup();

    // All declarations, including built-ins, pass through this classifier.
    let mut roots = CredentialRoots::default();
    for decl in &declarations {
        let root = &decl.root;
        if root.scope == CredentialScope::Exact {
            let path = resolve_declared(&root.path, &root.source)?;
            if lanes
                .iter()
                .any(|lane| root.path.starts_with(lane) || path.starts_with(lane))
            {
                let detail = format!(
                    "{}: declared credential file {} lies inside lane root(s) {}",
                    root.source,
                    root.path.display(),
                    lanes
                        .iter()
                        .filter(|lane| root.path.starts_with(lane) || path.starts_with(lane))
                        .map(|lane| lane.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                if isolated {
                    return Err(format!(
                        "{}: declared credential file {} lies inside lane root",
                        root.source,
                        root.path.display()
                    ));
                }
                warn(detail);
            }
            roots.add_entry(root.clone());
            continue;
        }
        let resolved_store = resolve_declared(&root.path, &root.source)?;
        if lanes
            .iter()
            .any(|lane| root.path.starts_with(lane) || resolved_store.starts_with(lane))
        {
            let detail = format!(
                "{}: credential store {} is equal to or inside lane root(s) {}",
                root.source,
                resolved_store.display(),
                lanes
                    .iter()
                    .filter(|lane| root.path.starts_with(lane) || resolved_store.starts_with(lane))
                    .map(|lane| lane.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if isolated {
                return Err(format!(
                    "{}: credential store {} is equal to or inside lane root",
                    root.source,
                    resolved_store.display()
                ));
            }
            warn(detail);
        }
        roots.add_entry(root.clone());
        let inventory = match &decl.inventory {
            CredentialInventory::Adapter(adapter) => {
                if provenant_records::AdapterSpec::get(adapter)
                    .is_some_and(|spec| !spec.credential_files.is_empty())
                {
                    crate::accounts::exact_directory_credential_paths(
                        &root.path,
                        adapter,
                        &root.source,
                    )
                } else {
                    Ok(Vec::new())
                }
            }
            CredentialInventory::Files(files) => {
                if files.iter().any(|name| {
                    let path = Path::new(name);
                    path.as_os_str().is_empty()
                        || path.is_absolute()
                        || path
                            .components()
                            .any(|part| !matches!(part, std::path::Component::Normal(_)))
                }) {
                    return Err(format!(
                        "{} has an invalid credential file inventory",
                        root.source
                    ));
                }
                Ok(files.iter().map(|name| root.path.join(name)).collect())
            }
            CredentialInventory::Unknown => Ok(Vec::new()),
        };
        for path in inventory? {
            let resolved = resolve_declared(&path, &root.source)?;
            if lanes
                .iter()
                .any(|lane| path.starts_with(lane) || resolved.starts_with(lane))
            {
                let detail = format!(
                    "{}: declared credential file {} lies inside lane root(s) {}",
                    root.source,
                    path.display(),
                    lanes
                        .iter()
                        .filter(|lane| path.starts_with(lane) || resolved.starts_with(lane))
                        .map(|lane| lane.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                if isolated {
                    return Err(format!(
                        "{}: declared credential file {} lies inside lane root",
                        root.source,
                        path.display()
                    ));
                }
                warn(detail);
            }
            roots.add(
                path,
                CredentialScope::Exact,
                root.source.clone(),
                provenant_core::CredentialKind::Store,
            );
        }
    }
    let roots = roots.with_lane_roots(lanes);

    if isolated {
        bind_credential_roots(roots)
    } else {
        roots.with_aliases(|path| resolve_declared(path, "credential root"))
    }
}

/// Retain the dispatch flag credential checks after the route access is known.
/// Early parsing cannot make this decision because access may be route-specific.
pub(super) fn credential_access_grant_refusal(
    access: &run::LaneAccessRequest,
    credentials: &provenant_core::CredentialRoots,
    receipt: &provenant_records::LaneAccessReceipt,
    policy: &provenant_records::policy::Policy,
) -> Option<String> {
    use provenant_records::run::LaneAccessGrantKind as Kind;
    let mut used_provenance = vec![false; access.dispatch_grants.len()];
    for (kind, paths) in [
        (Kind::WriteRoot, &access.write_roots),
        (Kind::Attachment, &access.attachments),
        (Kind::ReadRoot, &access.read_roots),
    ] {
        for path in paths {
            let given = access
                .dispatch_grants
                .iter()
                .enumerate()
                .find_map(|(index, grant)| {
                    (!used_provenance[index] && grant.kind == kind && grant.path == *path).then(
                        || {
                            used_provenance[index] = true;
                            grant.given.clone()
                        },
                    )
                })
                .unwrap_or_else(|| path.display().to_string());
            if let Some(root) = credentials.refusal(
                path,
                provenant_core::CredentialScope::Subtree,
                kind.is_read(),
            ) {
                if receipt.access.is_isolated() {
                    return Some(format!(
                        "{} {} overlaps declared credential root {} (source: {})",
                        kind.flag(),
                        given,
                        root.path.display(),
                        root.source
                    ));
                }
                let detail = format!(
                    "{} {} overlaps declared credential root {} (declaration: {}; lanes.access={}, source: {})",
                    kind.flag(),
                    given,
                    root.path.display(),
                    root.source,
                    receipt.access.as_str(),
                    receipt.source.as_str()
                );
                if let provenant_records::policy::Effect::Warn(warning) = policy.check(
                    "CREDENTIAL_ROOTS_OVERLAP_NONE",
                    provenant_records::policy::Facts::violation(detail),
                ) {
                    warn(warning);
                }
            }
        }
    }
    None
}

/// Surface credential overlaps in the final provider profile with the resolved policy.
pub(super) fn warn_profile_credential_grants(
    profile: &provenant_confinement::permissions::ProviderProfileSpec,
    credentials: &provenant_core::CredentialRoots,
    receipt: &provenant_records::LaneAccessReceipt,
    policy: &provenant_records::policy::Policy,
) {
    if receipt.access.is_isolated() {
        return;
    }
    let reads = profile
        .process_executables
        .iter()
        .chain(&profile.runtime_reads)
        .chain(&profile.tool_exec_roots);
    let writes = profile
        .continuation_reads
        .iter()
        .chain(&profile.state_writes)
        .chain(&profile.tool_writes)
        .chain(profile.fabric_socket.iter());
    let mut reported = std::collections::BTreeSet::new();
    for (grant, read) in reads
        .map(|grant| (grant, true))
        .chain(writes.map(|grant| (grant, false)))
    {
        if let Some(root) = provenant_confinement::permissions::credential_overlap_for_grant(
            grant,
            read,
            credentials,
        ) && reported.insert((grant.path.clone(), root.path.clone()))
        {
            let detail = format!(
                "provider grant {} overlaps credential root {} (declaration: {}; lanes.access={}, source: {})",
                grant.path.display(),
                root.path.display(),
                root.source,
                receipt.access.as_str(),
                receipt.source.as_str()
            );
            if let provenant_records::policy::Effect::Warn(warning) = policy.check(
                "CREDENTIAL_ROOTS_OVERLAP_NONE",
                provenant_records::policy::Facts::violation(detail),
            ) {
                warn(warning);
            }
        }
    }
}

fn bind_credential_roots(
    roots: provenant_core::CredentialRoots,
) -> Result<provenant_core::CredentialRoots, String> {
    let sources: std::collections::BTreeMap<PathBuf, String> = roots
        .entries()
        .iter()
        .map(|root| (root.path.clone(), root.source.clone()))
        .collect();
    roots.with_aliases(|path| {
        provenant_confinement::permissions::resolve_boundary_path(path).map_err(|error| {
            match sources.get(path) {
                Some(source) => format!(
                    "{source} at {} cannot be resolved precisely: {error}",
                    path.display()
                ),
                None => error.to_string(),
            }
        })
    })
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
            "CARGO_HOME"
                | "CODEX_HOME"
                | "CLAUDE_CONFIG_DIR"
                | "GH_CONFIG_DIR"
                | "GIT_CONFIG_GLOBAL"
                | "GIT_CONFIG_NOSYSTEM"
                | "GIT_CONFIG_SYSTEM"
                | "XDG_CONFIG_HOME"
                | "XDG_DATA_HOME"
                | "XDG_STATE_HOME"
                | "XDG_CACHE_HOME"
        )
    {
        return host.provider_path_environment.get(name).cloned();
    }
    std::env::var_os(name)
}

/// Select the captured host agent only for host-access lane launches.
pub(super) fn lane_provider_var(
    registry: &Registry<'_>,
    name: &str,
    access: provenant_records::LaneAccess,
) -> Option<std::ffi::OsString> {
    if name == "SSH_AUTH_SOCK" && !access.is_isolated() {
        return registry.host.map_or_else(
            || std::env::var_os(name),
            |host| host.provider_path_environment.get(name).cloned(),
        );
    }
    host_provider_var(registry, name)
}

/// Host token delivery must respect the same typed GitHub exception as file
/// access; acquiring a token outside the lane cannot bypass an explicit denial.
pub(super) fn github_auth_permitted(
    plan: &ConfinedPlan,
    profile: &provenant_confinement::permissions::ProviderProfileSpec,
) -> bool {
    let resolved = profile.confinement.resolve_for(plan.permissions());
    resolved.github_permitted()
        && plan.permissions().policy.as_ref().is_none_or(|policy| {
            !resolved.deny_read().iter().any(|denied| {
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

/// Resolve one PATH policy for the request project and selected worktree.
/// Git-backed roots come from the shared, verified worktree inventory owner;
/// a linked checkout contributes both its primary project and selected root.
pub(in crate::run) fn filtered_requester_lane_path(
    host: Option<&run::HostLaunchContext>,
    project_path: &Path,
    worktree_path: &Path,
    utility: &provenant_confinement::UtilityRuntime,
    executor: &dyn provenant_os::exec::Executor,
    cancel: &provenant_core::CancelToken,
    binding: Option<&provenant_config::ProjectBinding>,
) -> Result<run::FilteredLanePath, String> {
    let (project, selected) =
        requester_path_roots(project_path, utility, executor, cancel, binding)?;
    let selected_worktree = if canonical(project_path)? == canonical(worktree_path)? {
        selected
    } else {
        requester_path_roots(worktree_path, utility, executor, cancel, binding)?.1
    };
    run::FilteredLanePath::for_requester(host, &project, &selected_worktree)
        .map_err(|error| error.to_string())
}

pub(super) fn registered_project_binding(
    roots: &provenant_core::Roots,
    project: provenant_core::ProjectId,
) -> Result<Option<provenant_config::ProjectBinding>, PrepareError> {
    let bindings = provenant_config::registered_projects(roots)
        .map_err(PrepareError::registered_project_inventory)?;
    Ok(bindings
        .into_iter()
        .find(|binding| binding.project_id() == project))
}

/// Return the verified primary project root and selected checkout root. A
/// non-checkout directory remains its own root; linked checkouts use their
/// registered binding or the common directory's verified inventory.
fn requester_path_roots(
    path: &Path,
    utility: &provenant_confinement::UtilityRuntime,
    executor: &dyn provenant_os::exec::Executor,
    cancel: &provenant_core::CancelToken,
    binding: Option<&provenant_config::ProjectBinding>,
) -> Result<(PathBuf, PathBuf), String> {
    let git = utility.git_program_path();
    let path = canonical(path)?;
    if !path
        .ancestors()
        .any(|ancestor| provenant_os::git::repository_marker(ancestor).is_some())
    {
        return Ok((path.clone(), path));
    }
    let common = provenant_os::git::repository_path_with_cancel(
        executor,
        git,
        &path,
        provenant_os::git::RepositoryPath::CommonDir,
        Some(cancel),
    )
    .map_err(|error| format!("cannot resolve requester PATH repository: {error}"))?;
    let selected = provenant_os::git::repository_path_with_cancel(
        executor,
        git,
        &path,
        provenant_os::git::RepositoryPath::Toplevel,
        Some(cancel),
    )
    .map_err(|error| format!("cannot resolve requester PATH worktree: {error}"))?;
    let private = provenant_os::git::repository_path_with_cancel(
        executor,
        git,
        &path,
        provenant_os::git::RepositoryPath::GitDir,
        Some(cancel),
    )
    .map_err(|error| format!("cannot resolve requester PATH private Git directory: {error}"))?;
    let common = canonical(&common)?;
    let private = canonical(&private)?;
    let project = if private == common {
        // Git proves this selected top-level owns the shared metadata directly.
        selected.clone()
    } else if let Some(binding) =
        binding.filter(|binding| canonical(binding.common_dir()).is_ok_and(|bound| bound == common))
    {
        // The utility owns bound layout resolution, including trusted local
        // bare provenance for a checkout-less common directory.
        let layout =
            provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))
                .and_then(|budget| {
                    utility.project_layout_for_binding(executor, binding, budget, cancel)
                })
                .map_err(|error| format!("cannot verify requester PATH project layout: {error}"))?;
        layout.project().unwrap_or(layout.root()).to_path_buf()
    } else {
        let layout =
            provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))
                .and_then(|budget| {
                    utility.project_layout_for_common(executor, &common, budget, cancel)
                })
                .map_err(|error| format!("cannot verify requester PATH project layout: {error}"))?;
        layout.project().unwrap_or(layout.root()).to_path_buf()
    };
    Ok((project, selected))
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

fn source_cwd_authority(message: String) -> SourceCwdError {
    provenant_records::policy::Policy::default().boundary_error(
        "CONFINEMENT_SOURCE_CWD_AUTHORITY",
        provenant_records::policy::Facts::violation(&message),
        SourceCwdError::Authority(message),
    )
}

fn source_admission_error(error: crate::run::SourceAdmissionError) -> SourceCwdError {
    provenant_records::policy::Policy::default().boundary_error(
        error.code,
        provenant_records::policy::Facts::violation(&error.reason),
        SourceCwdError::Authority(error.reason),
    )
}

fn source_warning_messages(
    admission: &crate::run::SourceAdmission,
    policy: &provenant_records::policy::Policy,
) -> Vec<Warning> {
    admission
        .warnings
        .iter()
        .filter_map(|warning| {
            match policy.check(
                "CONFINEMENT_SOURCE_AGENT_CONFIG_NONE",
                provenant_records::policy::Facts::violation(format!(
                    "source overlaps {} (source: {})",
                    warning.path.display(),
                    warning.source
                )),
            ) {
                provenant_records::policy::Effect::Warn(warning) => Some(warning),
                _ => None,
            }
        })
        .collect()
}

fn report_source_warnings(
    admission: &crate::run::SourceAdmission,
    registry: &Registry<'_>,
    task: &run::TaskRequest,
) {
    let project = task
        .profile
        .policy_project
        .or(registry.project_id)
        .map(|project| project.to_string())
        .unwrap_or_default();
    let policy = registry
        .roots
        .map_or_else(provenant_records::policy::Policy::default, |roots| {
            run::launch_policy(roots.state.as_path(), &project, task)
        });
    for warning in source_warning_messages(admission, &policy) {
        warn(warning);
    }
}

pub(super) fn source_cwd(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
) -> Result<PathBuf, SourceCwdError> {
    let cwd = registry
        .cwd
        .ok_or_else(|| SourceCwdError::Operational("working directory is unavailable".into()))?;
    let state = registry
        .state_root
        .ok_or_else(|| SourceCwdError::Operational("state root is unavailable".into()))?;
    let Some(source) = &task.access.source_root else {
        if task.access.source_identity.is_some() || task.access.source_live {
            return Err(source_cwd_authority(
                "graph source identity has no checkout".into(),
            ));
        }
        let admission =
            crate::run::admit_source_root(cwd, state, cwd, crate::run::SourceAdmissionMode::Live)
                .map_err(source_admission_error)?;
        report_source_warnings(&admission, registry, task);
        return Ok(admission.path);
    };
    if task.writer.is_some() || (!task.access.source_live && task.head.is_none()) {
        return Err(source_cwd_authority(
            "read-only source needs a pinned head and no writer".into(),
        ));
    }
    if task.access.source_live && !std::fs::metadata(source).is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(source_cwd_authority(
            "graph source checkout is not an existing directory".into(),
        ));
    }
    let mode = if task.access.source_live {
        crate::run::SourceAdmissionMode::Live
    } else {
        crate::run::SourceAdmissionMode::Registered
    };
    let admission =
        crate::run::admit_source_root(cwd, state, source, mode).map_err(source_admission_error)?;
    report_source_warnings(&admission, registry, task);
    if !task.access.source_live && admission.path != *source {
        return Err(source_cwd_authority(
            "graph source checkout changed location".into(),
        ));
    }
    let identity = source_identity(&admission.path).map_err(source_cwd_authority)?;
    if task.access.source_identity.as_ref() != Some(&identity) {
        return Err(source_cwd_authority(
            "graph source checkout identity changed".into(),
        ));
    }
    Ok(admission.path)
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

fn source_head_authority(error: SourceHeadError) -> SourceHeadError {
    provenant_records::policy::Policy::default().boundary_error(
        "CONFINEMENT_REQUIRE_SOURCE_HEAD_AUTHORITY",
        provenant_records::policy::Facts::violation(error.to_string()),
        error,
    )
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
        let expected = source_head(task).map_err(source_head_authority)?;
        if observed != Some(expected) {
            return Err(source_head_authority(SourceHeadError::Moved(expected)));
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
    #[error("{}", read_only_diagnostic_text(.0.as_ref()))]
    ProjectBindingDirectory(Box<provenant_records::diagnostic::Diagnostic>),
    #[error("{0}")]
    Other(String),
}

fn read_only_diagnostic_text(diagnostic: &provenant_records::diagnostic::Diagnostic) -> String {
    let mut message = diagnostic
        .detail
        .clone()
        .unwrap_or_else(|| "project binding state directory is unsafe".into());
    if let Some(next) = &diagnostic.next {
        message.push_str("; next: ");
        message.push_str(next);
    }
    message
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
impl From<provenant_config::ConfigError> for ReadOnlyPlanError {
    fn from(error: provenant_config::ConfigError) -> Self {
        let diagnostic =
            provenant_fabric_runtime::coordination::facade::FacadeError::project_binding_directory_diagnostic(
                &error,
                provenant_records::diagnostic::Destination::Local,
            );
        diagnostic.map_or_else(
            || Self::Other(error.to_string()),
            |diagnostic| Self::ProjectBindingDirectory(Box::new(diagnostic)),
        )
    }
}
impl ReadOnlyPlanError {
    pub(super) fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass;
        match self {
            Self::Run(error) => error.boundary_class(),
            Self::Plan(error) => error.boundary_class(),
            Self::BindingMismatch
            | Self::MissingBinding
            | Self::PinnedSourceRequiresGit
            | Self::ProjectBindingDirectory(_) => BoundaryClass::Authority,
            Self::Other(_) => BoundaryClass::None,
        }
    }
}

fn read_only_plan_authority(error: ReadOnlyPlanError) -> ReadOnlyPlanError {
    if error.boundary_class() == provenant_records::diagnostic::BoundaryClass::Authority {
        provenant_records::policy::Policy::default().boundary_error(
            "CONFINEMENT_READ_ONLY_PLAN_AUTHORITY",
            provenant_records::policy::Facts::violation(error.to_string()),
            error,
        )
    } else {
        error
    }
}

// Spec: docs/specs/state.md#config-never-widens-authority
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
    let unsafe_binding = ReadOnlyPlanError::from(
        provenant_config::ConfigError::ProjectBindingDirectoryUnsafe {
            path: PathBuf::from("/private/state"),
            mode: 0o755,
            owner_matches: true,
        },
    );
    assert!(matches!(
        &unsafe_binding,
        ReadOnlyPlanError::ProjectBindingDirectory(diagnostic)
            if diagnostic.detail.as_deref().is_some_and(|detail| detail.contains("/private/state"))
                && diagnostic.next.as_deref().is_some_and(|next| next.contains("chmod 700"))
    ));
    assert_eq!(unsafe_binding.boundary_class(), BoundaryClass::Authority);
    assert!(matches!(
        ReadOnlyPlanError::from(provenant_config::ConfigError::InvalidProjectBinding),
        ReadOnlyPlanError::Other(_)
    ));
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

    let result = (|| {
        // The host-pinned Git, the same one the lane receives as PROVENANT_GIT.
        let utility_home = host_git_home(roots.state.as_path())?;
        let utility = UtilityRuntime::new(git, utility_home.path(), roots.state.as_path())
            .map_err(|error| error.to_string())?;
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
        let location = match repository.as_ref() {
            Some(repo) if repo.private_git_dir() == repo.common_git_dir() => {
                provenant_config::ProjectLocation::from_verified_primary_checkout(
                    repo.common_git_dir(),
                    repo.root_path(),
                )
            }
            Some(repo) => provenant_config::ProjectLocation::new(repo.common_git_dir()),
            None => provenant_config::ProjectLocation::new(cwd),
        }
        .map_err(|error| error.to_string())?;
        let binding = if live {
            // Graph attribution stays with the parent; the read plan belongs to
            // the selected checkout, as it does for an ordinary host launch.
            let binding =
                provenant_config::bind_project(roots, location).map_err(ReadOnlyPlanError::from)?;
            request.scope = WorkScope::Project(binding.project_id());
            binding
        } else {
            let binding = provenant_config::lookup_project(roots, &location)
                .map_err(ReadOnlyPlanError::from)?
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
                    Some(
                        provenant_core::GitOid::parse(raw.trim())
                            .map_err(|error| error.to_string())?,
                    )
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
    })();
    result.map_err(read_only_plan_authority)
}

/// Advisory freshness of the checkout a read-only lane reads, for the launch
/// warnings. The lane reads `source`: the `--worktree` checkout, or the caller's
/// checkout when none is named. Absent or unusable Git data yields no warning.
pub(in crate::run) fn read_only_checkout_warning(
    state: &Path,
    source: &Path,
    choice: &crate::run::ReadSourceChoice,
) -> Option<String> {
    let selected = resolve_read_source(state, source, choice)?;
    Some(crate::run::read_only_local_checkout_warning(
        source, &selected,
    ))
}

/// Run a bounded host-side fetch so a fetch-first source resolves a ref that
/// the checkout's last fetch did not yet have. This runs on the host at launch,
/// never inside a lane, and uses the host's own Git and credential settings.
fn fetch_read_source(remote: &provenant_config::RemoteName, repo: &Path) -> bool {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};

    let Some(git) = provenant_os::git::git_executable() else {
        return false;
    };
    let Ok(executor) = SystemExecutor::new(1) else {
        return false;
    };
    let argv: Vec<std::ffi::OsString> = vec![
        git.as_os_str().to_owned(),
        std::ffi::OsString::from("--no-pager"),
        std::ffi::OsString::from("fetch"),
        std::ffi::OsString::from("--no-tags"),
        std::ffi::OsString::from("--quiet"),
        std::ffi::OsString::from(remote.as_str()),
    ];
    let env: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            let key = key.to_string_lossy();
            key != "GIT_TERMINAL_PROMPT" && key != "GIT_ASKPASS"
        })
        .chain([(
            std::ffi::OsString::from("GIT_TERMINAL_PROMPT"),
            std::ffi::OsString::from("0"),
        )])
        .collect();
    let mut options = ExecOptions::new(&argv, repo, std::time::Duration::from_secs(60));
    options.env = Some(&env);
    executor
        .execute(options)
        .map(|out| out.termination == Termination::Exit(0))
        .unwrap_or(false)
}

/// The one resolver for a read-only lane's source. The compiled default pins
/// the fetched upstream when the checkout is behind it, and pins the observed
/// checkout HEAD otherwise, so a commit after admission cannot move the tree a
/// read-only lane reads; `live` keeps the local tree, `default_branch` the
/// fetched default, and any other value an explicit ref. `fetch_first` fetches
/// first; unusable Git evidence is advisory, never fatal.
pub(in crate::run) fn resolve_read_source(
    state: &Path,
    source: &Path,
    choice: &crate::run::ReadSourceChoice,
) -> Option<crate::run::ReadSourcePin> {
    use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime, run_read_utility};
    use provenant_os::exec::SystemExecutor;

    if choice.source == crate::run::ReadSource::Live {
        return None;
    }
    let git = provenant_os::git::git_executable()?;
    let utility_home = host_git_home(state).ok()?;
    let utility = UtilityRuntime::new(&git, utility_home.path(), state).ok()?;
    let executor = SystemExecutor::new(1).ok()?;
    let budget = UtilityBudget::bounded(std::time::Duration::from_secs(5)).ok()?;
    let cancel = provenant_core::CancelToken::new();
    let ancestor = source.ancestors().find(|path| path.join(".git").exists())?;
    if choice.fetch_first
        && let Some(remote) = resolved_forge_remote(state, ancestor)
        && !fetch_read_source(&remote, ancestor)
    {
        warn(Warning::new(
            provenant_records::warning::UPSTREAM_UNRESOLVED,
            format!(
                "lanes.read_source_fetch_first: fetch from {remote} did not complete; resolving {} from the last fetched refs",
                choice.source
            ),
        ));
    }
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
    let (commit, reference) = if choice.source == crate::run::ReadSource::Auto {
        configured_upstream(&executor, &repo, &utility, budget, &cancel).or_else(|| {
            // The remote comes from the one declared/inferred context, never a
            // compiled `origin`: a fork checkout must not pin its review base to
            // the fork's default when `upstream` is the real forge. An
            // unresolvable or ambiguous remote keeps the local-source behaviour.
            // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
            let remote = resolved_forge_remote(state, ancestor)?;
            let fallback =
                default_upstream(&executor, &repo, &utility, budget, &cancel, &remote)?;
            // The branch's own tracking configuration could not be resolved, so
            // the remote's default branch is only an advisory stand-in.
            warn(Warning::new(
                provenant_records::warning::UPSTREAM_UNRESOLVED,
                format!("read-only source could not resolve this branch's configured upstream; using {} instead, which may not track this checkout", fallback.1),
            ));
            Some(fallback)
        }).unwrap_or_else(|| {
            let reference = current_branch(&executor, &repo, &utility, budget, &cancel)
                .unwrap_or_else(|| "HEAD".to_owned());
            (head, reference)
        })
    } else if choice.source == crate::run::ReadSource::DefaultBranch {
        // An explicit default_branch pins the fetched default branch, never
        // this checkout's configured tracking upstream: a feature branch may
        // track a different branch and must still read the forge default.
        // Only the auto choice above uses configured_upstream.
        // Spec: docs/specs/run-launch.md#inputs-and-resources
        let remote = resolved_forge_remote(state, ancestor)?;
        default_upstream(&executor, &repo, &utility, budget, &cancel, &remote)?
    } else {
        resolve_upstream_ref(
            &executor,
            &repo,
            &utility,
            budget,
            &cancel,
            &choice.source.to_string(),
        )?
    };
    let behind =
        commits_behind(&executor, &repo, &utility, budget, &cancel, head, commit).unwrap_or(0);
    if choice.source == crate::run::ReadSource::Auto && behind == 0 {
        // An up-to-date auto checkout still pins the HEAD observed here: the
        // tree a read-only lane reads must not move after admission. The
        // reference names the checked-out branch, or HEAD when detached.
        let reference = current_branch(&executor, &repo, &utility, budget, &cancel)
            .unwrap_or_else(|| "HEAD".to_owned());
        return Some(crate::run::ReadSourcePin {
            commit: head,
            reference,
            behind: 0,
            checkout_head: head,
        });
    }
    Some(crate::run::ReadSourcePin {
        commit,
        reference,
        behind,
        checkout_head: head,
    })
}

/// Resolve a fetched ref to its commit and keep the exact ref that named it.
fn resolve_upstream_ref(
    executor: &dyn provenant_os::exec::Executor,
    repo: &provenant_confinement::ReadOnlyRepo,
    utility: &provenant_confinement::UtilityRuntime,
    budget: provenant_confinement::UtilityBudget,
    cancel: &provenant_core::CancelToken,
    reference: &str,
) -> Option<(provenant_core::GitOid, String)> {
    use provenant_confinement::{GitRead, run_read_utility};
    use provenant_os::exec::Termination;

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
    let oid =
        provenant_core::GitOid::parse(std::str::from_utf8(&out.stdout.bytes).ok()?.trim()).ok()?;
    Some((oid, reference.to_owned()))
}

/// The short name of the checked-out branch, when HEAD is symbolic.
fn current_branch(
    executor: &dyn provenant_os::exec::Executor,
    repo: &provenant_confinement::ReadOnlyRepo,
    utility: &provenant_confinement::UtilityRuntime,
    budget: provenant_confinement::UtilityBudget,
    cancel: &provenant_core::CancelToken,
) -> Option<String> {
    use provenant_confinement::{GitRead, run_read_utility};
    use provenant_os::exec::Termination;

    let out = run_read_utility(
        executor,
        GitRead::SymbolicRevision {
            revision: provenant_confinement::repo::GitRevision::new("HEAD").ok()?,
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
    std::str::from_utf8(&out.stdout.bytes)
        .ok()?
        .trim()
        .strip_prefix("refs/heads/")
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
}

/// One branch remote or merge value from the verified repository config.
/// Missing values are normal; other failures are reported before falling back.
fn branch_config_value(
    executor: &dyn provenant_os::exec::Executor,
    repo: &provenant_confinement::ReadOnlyRepo,
    utility: &provenant_confinement::UtilityRuntime,
    budget: provenant_confinement::UtilityBudget,
    cancel: &provenant_core::CancelToken,
    key: &str,
) -> Option<String> {
    use provenant_confinement::{
        run_verified_git_read_command, utility::validate_branch_config_key,
    };
    use provenant_os::exec::Termination;

    if validate_branch_config_key(key).is_err() {
        warn(Warning::new(
            provenant_records::warning::BRANCH_CONFIG_UNREADABLE,
            "read branch Git configuration rejected an invalid key",
        ));
        return None;
    }
    let context = provenant_os::git_config::ConfigContext {
        operation: "read branch Git configuration",
        file: repo.common_git_dir().join("config"),
        key: Some("branch tracking key"),
    };
    let out = match provenant_os::git_config::read(
        context,
        provenant_os::git_config::Read::GetAfterOptions(key),
        |command| run_verified_git_read_command(executor, command, repo, utility, budget, cancel),
    ) {
        Ok(out) => out,
        Err(error) => {
            warn(Warning::new(
                provenant_records::warning::BRANCH_CONFIG_UNREADABLE,
                error.to_string(),
            ));
            return None;
        }
    };
    match out.termination {
        Termination::Exit(0) => {}
        Termination::Exit(1) => return None,
        termination => {
            warn(Warning::new(
                provenant_records::warning::BRANCH_CONFIG_UNREADABLE,
                format!("read branch Git configuration ended with {termination:?}"),
            ));
            return None;
        }
    }
    let value = match std::str::from_utf8(&out.stdout.bytes) {
        Ok(value) => value.trim(),
        Err(error) => {
            warn(Warning::new(
                provenant_records::warning::BRANCH_CONFIG_UNREADABLE,
                format!("parse branch Git configuration failed: {error}"),
            ));
            return None;
        }
    };
    (!value.is_empty()).then(|| value.to_owned())
}

/// The fetched remote-tracking head configured for the current branch, by way
/// of `branch.<name>.remote` and `branch.<name>.merge`. A detached HEAD, a
/// local-only upstream or an unfetched ref yields no pin here.
fn configured_upstream(
    executor: &dyn provenant_os::exec::Executor,
    repo: &provenant_confinement::ReadOnlyRepo,
    utility: &provenant_confinement::UtilityRuntime,
    budget: provenant_confinement::UtilityBudget,
    cancel: &provenant_core::CancelToken,
) -> Option<(provenant_core::GitOid, String)> {
    use provenant_confinement::repo::GitRefName;

    let branch = current_branch(executor, repo, utility, budget, cancel)?;
    let remote = branch_config_value(
        executor,
        repo,
        utility,
        budget,
        cancel,
        &format!("branch.{branch}.remote"),
    )?;
    let merge = branch_config_value(
        executor,
        repo,
        utility,
        budget,
        cancel,
        &format!("branch.{branch}.merge"),
    )?;
    let short = merge.strip_prefix("refs/heads/")?;
    let name = GitRefName::remote_tracking(format!("{remote}/{short}")).ok()?;
    resolve_upstream_ref(executor, repo, utility, budget, cancel, name.as_str())
}

/// The one remote for an advisory read-only source pin, from the full port
/// table (host and project declarations together). A fork's `origin` is never
/// assumed: resolution yields a typed name, or absence, never a blank `""` Git
/// could fetch; callers without a remote keep their local source behaviour.
///
/// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
fn resolved_forge_remote(state: &Path, project: &Path) -> Option<provenant_config::RemoteName> {
    let ports = provenant_config::PortTable::read_for(Some(project), state).ok()?;
    let policy = provenant_config::operation_policy(state, project, &ports);
    let config = provenant_config::ForgeConfig::new(&ports, (*policy).clone());
    let git = provenant_config::ForgeGit::new(project).ok()?;
    match provenant_config::ForgeContext::remote_if_any(&config, &git) {
        Ok(remote) => remote,
        Err(provenant_config::ForgeContextError::InvalidRemote { remote }) => {
            warn(
                Warning::new(
                    provenant_records::warning::FORGE_CONTEXT_UNRESOLVED,
                    format!("configured remote {remote:?} is not a usable Git remote name; keeping the local source behaviour"),
                )
                .once_per_change(),
            );
            None
        }
        Err(_) => None,
    }
}

/// The remote's default branch as last fetched, with the exact ref that named
/// it. Advisory fallback only: this never fetches. The remote is asked only
/// when no tracking ref is recorded for the resolved remote, and only within
/// the utility budget.
///
/// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
fn default_upstream(
    executor: &dyn provenant_os::exec::Executor,
    repo: &provenant_confinement::ReadOnlyRepo,
    utility: &provenant_confinement::UtilityRuntime,
    budget: provenant_confinement::UtilityBudget,
    cancel: &provenant_core::CancelToken,
    remote: &provenant_config::RemoteName,
) -> Option<(provenant_core::GitOid, String)> {
    use provenant_confinement::utility::advertised_default_branch;

    resolve_upstream_ref(
        executor,
        repo,
        utility,
        budget,
        cancel,
        &format!("refs/remotes/{remote}/HEAD"),
    )
    .or_else(|| {
        let branch =
            advertised_default_branch(executor, repo, utility, remote.as_str(), budget, cancel)
                .ok()??;
        resolve_upstream_ref(
            executor,
            repo,
            utility,
            budget,
            cancel,
            &format!("refs/remotes/{remote}/{}", branch.short_name()),
        )
    })
}

/// How many commits of `head` are not in `base`, as last fetched. Zero means
/// the checkout is level with or ahead of the default branch.
fn commits_behind(
    executor: &dyn provenant_os::exec::Executor,
    repo: &provenant_confinement::ReadOnlyRepo,
    utility: &provenant_confinement::UtilityRuntime,
    budget: provenant_confinement::UtilityBudget,
    cancel: &provenant_core::CancelToken,
    head: provenant_core::GitOid,
    base: provenant_core::GitOid,
) -> Option<u64> {
    use provenant_confinement::{GitRead, run_read_utility};
    use provenant_os::exec::Termination;

    let behind = run_read_utility(
        executor,
        GitRead::AncestorCount {
            base: head,
            head: base,
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
}

/// Publishing hands the lane the GitHub credential. It is on by default for
/// every lane on open network, read-only lanes included; `lanes.publish=false`
/// withholds it. A missing or
/// non-boolean value takes the registry default.
pub(super) fn publishing_requested(
    open_network: bool,
    knobs: &std::collections::BTreeMap<String, serde_json::Value>,
) -> bool {
    provenant_records::run::host_publishing_requested(
        if open_network {
            NetworkAccess::Open
        } else {
            NetworkAccess::Provider
        },
        knobs,
    )
}

pub(super) fn publishing_executable(
    locate: &impl Fn(&str) -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    // Resolve links before adding exact executable grants or writing wrappers.
    let executable = canonical(&locate("gh")?)?;
    if executable.is_file() && provenant_os::sys::is_executable(&executable) {
        Ok(executable)
    } else {
        Err("host gh is not an executable file".into())
    }
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

/// Generated files and links stay in scratch or the attempt's private state
/// (a lane home, such as Codex's, lives there).
/// Directories may use any run-owned root, including trusted shared state, and
/// the selected Claude account's `projects/` tree. Traversal never follows links.
pub(super) fn apply_setup(
    steps: &[SetupStep],
    roots: &[&Path],
    claude_account: Option<&Path>,
) -> Result<(), String> {
    for step in steps {
        match step {
            // Setup names a lane home by creating its parent, and that parent
            // can be a run-owned root, which exists by construction.
            SetupStep::CreateDir(path) if roots.contains(&path.as_path()) => {}
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
            SetupStep::CreateLock(path) => {
                let (parent, name) = setup_parent(&roots[..roots.len().min(2)], path, false)?;
                parent
                    .try_lock(name, provenant_os::fs::LockMode::Exclusive)
                    .map_err(|error| error.to_string())?;
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
    let (parent, name) = setup_parent(&roots[..roots.len().min(2)], path, false)?;
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

// Existing shared ancestors need only read access in a confined lane.
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
    policy: &provenant_records::policy::Policy,
) -> Result<Option<String>, PrepareError> {
    let boundary_error = |error: PrepareError| {
        policy.boundary_error(
            "CONFINEMENT_WRITER_SOURCE_CONTEXT_AUTHORITY",
            provenant_records::policy::Facts::violation(error.to_string()),
            error,
        )
    };
    // A pin mismatch gets its own cause text naming the pinned commit and the
    // current head, never the generic launch-handoff wording.
    let pin_error = |error: run::RunError, uncommitted: &[String]| -> PrepareError {
        match error {
            run::RunError::WriterSourceMismatch { expected, actual } => {
                PrepareError::WriterPinMismatch {
                    pinned: expected,
                    head: actual,
                    uncommitted: uncommitted.to_vec(),
                }
            }
            other => PrepareError::WriterContinuation(other.to_string()),
        }
    };
    let Some(source_head) = task.source_head.as_deref() else {
        if let Some(head) = task.head.as_deref() {
            let head = head.parse().map_err(|_| {
                boundary_error(PrepareError::WriterContinuation(
                    "invalid pinned writer source".into(),
                ))
            })?;
            writer
                .require_source(head, policy)
                .map_err(|error| boundary_error(pin_error(error, &[])))?;
        }
        return Ok(None);
    };
    let uncommitted = writer.uncommitted_paths(git).map_err(|error| {
        boundary_error(PrepareError::WriterContinuation(format!(
            "cannot inspect uncommitted predecessor edits: {error}"
        )))
    })?;
    let predecessor = task
        .head
        .as_deref()
        .ok_or_else(|| {
            boundary_error(PrepareError::WriterContinuation(
                "missing predecessor head for writer continuation".into(),
            ))
        })?
        .parse()
        .map_err(|_| {
            boundary_error(PrepareError::WriterContinuation(
                "invalid pinned writer predecessor head".into(),
            ))
        })?;
    let source = source_head.parse().map_err(|_| {
        boundary_error(PrepareError::WriterContinuation(
            "invalid pinned writer source".into(),
        ))
    })?;
    if let Err(error) = writer.require_continuation_source(git, source, predecessor, policy) {
        return Err(boundary_error(pin_error(error, &uncommitted)));
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
    // Spec: docs/specs/fabric.md#sources-and-read-roots
    fn source_configuration_warning_is_opt_in() {
        use provenant_records::policy::{Policy, Source, Strength};
        let admission = crate::run::SourceAdmission {
            path: "/project".into(),
            warnings: vec![crate::run::SourceWarning {
                path: "/project/.claude".into(),
                source: "project-local agent configuration (.claude)".into(),
            }],
        };
        let mut policy = Policy::default();
        assert!(source_warning_messages(&admission, &policy).is_empty());
        policy.overlay(
            &std::collections::BTreeMap::from([(
                "CONFINEMENT_SOURCE_AGENT_CONFIG_NONE".into(),
                Strength::Warn,
            )]),
            Source::Host,
        );
        assert_eq!(source_warning_messages(&admission, &policy).len(), 1);
    }

    #[test]
    fn only_the_temporary_variables_move_to_the_alias() {
        let scratch = Path::new("/state/attempt-scratch/a/tmp/candidate-0");
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
        assert!(!Path::new(&env[0].1).starts_with("/state/attempts"));
        assert_eq!(env[1].1, alias.join("zsh").as_os_str());
        assert_eq!(env[2].1, scratch.join("target").as_os_str());
        assert_eq!(env[3].1, scratch.as_os_str());
    }
}

#[cfg(test)]
mod config_warning_tests {
    #[derive(Debug)]
    struct RepeatingWrapper(std::io::Error);

    impl std::fmt::Display for RepeatingWrapper {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "wrapper repeats its source: {}", self.0)
        }
    }

    impl std::error::Error for RepeatingWrapper {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn config_warning_keeps_context_and_one_leaf_cause() {
        let error = provenant_os::git_config::ConfigError {
            context: provenant_os::git_config::ConfigContext::new(
                "read branch Git configuration",
                "/p/.git/config",
                Some("branch tracking key"),
            ),
            source: RepeatingWrapper(std::io::Error::other("deadline marker")),
        };
        let rendered = error.to_string();
        assert!(
            rendered.contains(
                "read branch Git configuration on /p/.git/config for key branch tracking key"
            ),
            "{rendered}"
        );
        assert_eq!(rendered.matches("deadline marker").count(), 1, "{rendered}");
        assert!(
            !rendered.contains("wrapper repeats its source"),
            "{rendered}"
        );
    }
}
