//! Provider preparation at the CLI composition edge.
use super::{Registry, route_request};
use provenant_confinement::permissions::{
    AccessMode, PathGrant, PathScope, PlanPermissions, WorktreeWriteAuthority,
};
use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
use provenant_core::{ProjectId, WorkScope};
use provenant_providers::{LaneRuntime, PrepareInput, Prompt, REGISTRY, SetupStep, Stdin};
use provenant_records::{ResolvedRoute, RouteAlternative};
use provenant_run as run;
use provenant_supervisor::fs::{Directory, entropy_128};
use run::RouteRecord;
use std::path::{Path, PathBuf};

pub(super) fn prepare(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt_text: &str,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
) -> Result<run::PreparedLaunch, String> {
    prepare_with(
        registry,
        task,
        prompt_text,
        paths,
        resume,
        resolve_executable,
        None,
    )
}

fn prepare_with(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt_text: &str,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
    executable: impl Fn(&str) -> Result<PathBuf, String>,
    token: Option<&provenant_providers::claude::OauthToken>,
) -> Result<run::PreparedLaunch, String> {
    if resume.is_some() {
        return Err(
            "continuation needs a provider session; providers do not expose sessions yet".into(),
        );
    }
    let roots = registry.roots.ok_or("routing context is unavailable")?;
    let home = canonical(registry.home.ok_or("home directory is unavailable")?)?;
    let state_root = registry.state_root.ok_or("state root is unavailable")?;
    let assets = registry.assets.ok_or("product assets are unavailable")?;
    let provenant_executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let provenant_executable = canonical(&provenant_executable)?;
    let project_id = registry
        .project_id
        .ok_or("selected project identity is unavailable")?;
    let writer = match &task.writer {
        Some(request) => Some(
            run::prepare_writer(request, state_root, project_id)
                .map_err(|error| error.to_string())?,
        ),
        None => None,
    };
    let cwd = match writer.as_ref() {
        Some(writer) => writer.worktree.path().to_owned(),
        None => canonical(registry.cwd.ok_or("working directory is unavailable")?)?,
    };
    let prompt = Prompt::new(prompt_text.to_owned());
    prompt.check_argv().map_err(|error| error.to_string())?;
    let user = provenant_supervisor::sys::account(provenant_supervisor::sys::Account::Current)
        .map_err(|error| error.to_string())?
        .ok_or("current user identity is unavailable")?
        .name;

    let resolved = crate::route::resolve(&route_request(task)?, roots, task.account.as_deref())?;
    let mut reasons = Vec::new();
    for (candidate, route) in candidates(resolved).into_iter().enumerate() {
        let label = format!("{}/{}", route.adapter, route.model);
        let Some(registration) = REGISTRY
            .iter()
            .find(|registration| registration.adapter.id() == route.adapter)
        else {
            reasons.push(format!("{label}: unknown adapter"));
            continue;
        };
        let adapter = registration.adapter;
        if !adapter.execution_enabled() {
            reasons.push(format!("{label}: provider execution is disabled"));
            continue;
        }
        if adapter.id() == "claude" && token.is_none() {
            reasons.push(format!(
                "{label}: Claude credential authority is not available in this revision"
            ));
            continue;
        }
        if !matches!(adapter.id(), "codex" | "claude") {
            reasons.push(format!("{label}: provider launch is not supported here"));
            continue;
        }
        let executable = match executable(adapter.id()) {
            Ok(path) => path,
            Err(error) => {
                reasons.push(format!("{label}: {error}"));
                continue;
            }
        };
        // A failed setup never poisons the next explicit fallback candidate.
        let scratch = paths.scratch.join(format!("candidate-{candidate}"));
        let private_state = paths.provider_state.join(format!("candidate-{candidate}"));
        create_directory(&paths.scratch, &scratch)?;
        create_directory(&paths.provider_state, &private_state)?;
        let (scratch, private_state) = (scratch.as_path(), private_state.as_path());
        let child_state = state_root
            .join("children")
            .join(paths.attempt_id.to_string());
        let plan = match confinement_plan(
            project_id,
            &cwd,
            scratch,
            private_state,
            writer.as_ref(),
            (adapter.id() == "codex").then_some("login"),
        ) {
            Ok(plan) => plan,
            Err(error) => {
                reasons.push(format!("{label}: {error}"));
                continue;
            }
        };
        let nonce = entropy_128().map_err(|error| error.to_string())?;
        let grant_digest = task.authority_digest.to_string();
        let spec = match adapter.prepare(PrepareInput {
            route: &route,
            confined: &plan,
            prompt: &prompt,
            continuation: None,
            grant_digest: &grant_digest,
            executable: &executable,
            home: &home,
            user: &user,
            account: None,
            token,
            cwd: &cwd,
            nonce,
            provenant: LaneRuntime {
                executable: &provenant_executable,
                state: roots.state.as_path(),
                child_state: &child_state,
                home: &home,
                depth: paths.lane_depth,
                attempt: paths.attempt_id,
                assets,
            },
        }) {
            Ok(spec) => spec,
            Err(error) => {
                reasons.push(format!("{label}: {error}"));
                continue;
            }
        };
        if let Err(error) = apply_setup(&spec.setup, scratch, private_state, roots.state.as_path())
        {
            reasons.push(format!("{label}: setup failed: {error}"));
            continue;
        }
        let provider = RouteRecord {
            model: Some(format!("{}/{}", route.adapter, route.model)),
            provider: Some(route.adapter),
            effort: route.effort,
            account_label: None,
        };
        let stdin = match spec.stdin {
            Stdin::Bytes(bytes) => bytes,
            Stdin::Null => Vec::new(),
        };
        return Ok(run::PreparedLaunch {
            argv: spec.argv,
            environment: spec.env,
            stdin,
            cwd: spec.cwd,
            route: provider,
            confinement: Some(run::LaunchConfinement {
                plan,
                profile: spec.profile,
                writer,
            }),
        });
    }
    Err(reasons.join("; "))
}

fn candidates(resolved: ResolvedRoute) -> Vec<ResolvedRoute> {
    let mut routes = vec![resolved.clone()];
    routes.extend(
        resolved
            .alternatives
            .as_slice()
            .iter()
            .map(|alternative| with_alternative(&resolved, alternative)),
    );
    routes
}

fn with_alternative(base: &ResolvedRoute, alternative: &RouteAlternative) -> ResolvedRoute {
    ResolvedRoute {
        adapter: alternative.adapter.clone(),
        model: alternative.model.clone(),
        effort: alternative.effort.clone(),
        catalogue_revision: base.catalogue_revision.clone(),
        account_key: alternative.account_key.clone(),
        host: alternative.host.clone(),
        alternatives: Default::default(),
        exclusions: base.exclusions.clone(),
        eligibility: alternative.eligibility.clone(),
        readiness: alternative.readiness.clone(),
        confidentiality: alternative.confidentiality.clone(),
    }
}

fn confinement_plan(
    project_id: ProjectId,
    cwd: &Path,
    scratch: &Path,
    private_state: &Path,
    writer: Option<&run::WriterLaunch>,
    authentication: Option<&str>,
) -> Result<ConfinedPlan, String> {
    let mode = match writer {
        Some(writer) => AccessMode::WorktreeWrite(Box::new(
            WorktreeWriteAuthority::for_worktree(&writer.worktree, &writer.branch)
                .map_err(|error| error.to_string())?,
        )),
        None => AccessMode::ReadOnly,
    };
    let grant = |path: &Path| PathGrant {
        path: path.to_owned(),
        scope: PathScope::Subtree,
    };
    ConfinedPlan::new(PlanRequest {
        scope: WorkScope::Project(project_id),
        permissions: PlanPermissions {
            mode,
            scratch: grant(scratch),
            private_state: Some(grant(private_state)),
            read_roots: if writer.is_some() {
                Vec::new()
            } else {
                vec![grant(cwd)]
            },
            protected_roots: vec![PathGrant {
                path: cwd.join("AGENTS.md"),
                scope: PathScope::Exact,
            }],
            secret_roots: Vec::new(),
            selected_authentication: authentication.map(str::to_owned),
            include_continuation: false,
            network: Vec::new(),
            capabilities: Vec::new(),
            nested_confinement: false,
        },
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
    .map_err(|error| error.to_string())
}

fn resolve_executable(name: &str) -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").ok_or("PATH is unavailable")?;
    for directory in std::env::split_paths(&path).filter(|directory| directory.is_absolute()) {
        let candidate = directory.join(name);
        if candidate.is_file() && provenant_supervisor::sys::is_executable(&candidate) {
            return canonical(&candidate);
        }
    }
    Err(format!("{name} executable was not found on PATH"))
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    provenant_supervisor::fs::canonicalize(path).map_err(|error| error.to_string())
}

fn apply_setup(
    steps: &[SetupStep],
    scratch: &Path,
    private_state: &Path,
    state_root: &Path,
) -> Result<(), String> {
    for step in steps {
        match step {
            SetupStep::CreateDir(path) => {
                let directory = create_directory(state_root, path)?;
                directory.sync().map_err(|error| error.to_string())?;
            }
            SetupStep::WriteFile { path, bytes } => {
                let name = path
                    .file_name()
                    .ok_or("generated file has no final component")?;
                let parent_path = path.parent().ok_or("generated file has no parent")?;
                let parent = open_directory(scratch, parent_path)?;
                let mut file = parent.create_new(name).map_err(|error| error.to_string())?;
                use std::io::Write;
                file.write_all(bytes).map_err(|error| error.to_string())?;
                file.sync().map_err(|error| error.to_string())?;
                parent.sync().map_err(|error| error.to_string())?;
            }
            SetupStep::Symlink { link, target } => {
                if link.parent() != Some(private_state) {
                    return Err("provider setup link is outside private state".into());
                }
                let name = link
                    .file_name()
                    .ok_or("provider setup link has no final component")?;
                let parent =
                    Directory::open_anchor(private_state).map_err(|error| error.to_string())?;
                parent
                    .symlink(target, name)
                    .map_err(|error| error.to_string())?;
                parent.sync().map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

fn open_directory(root: &Path, path: &Path) -> Result<Directory, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "setup path is outside its granted root")?;
    let mut directory = Directory::open_anchor(root).map_err(|error| error.to_string())?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err("setup path has an invalid component".into());
        };
        directory = directory
            .open_directory(name)
            .map_err(|error| error.to_string())?;
    }
    Ok(directory)
}

fn create_directory(root: &Path, path: &Path) -> Result<Directory, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "setup directory is outside its granted root")?;
    let mut directory = Directory::open_anchor(root).map_err(|error| error.to_string())?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err("setup directory has an invalid component".into());
        };
        match directory.open_directory(name) {
            Ok(child) => directory = child,
            Err(error) if error.is_not_found() => {
                directory
                    .mkdir(name, 0o700)
                    .map_err(|error| error.to_string())?;
                directory.sync().map_err(|error| error.to_string())?;
                directory = directory
                    .open_directory(name)
                    .map_err(|error| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(directory)
}

/// Fixture access to the production plan builder; installed builds omit it.
#[cfg(feature = "fixture")]
pub fn writer_fixture_plan(
    project: ProjectId,
    cwd: &Path,
    paths: &run::LaunchPaths,
    writer: &run::WriterLaunch,
) -> Result<ConfinedPlan, String> {
    confinement_plan(
        project,
        cwd,
        &paths.scratch,
        &paths.provider_state,
        Some(writer),
        Some("login"),
    )
}

/// Exercise production preparation against a fake external provider executable.
#[cfg(feature = "fixture")]
pub fn prepare_fixture(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt: &str,
    paths: &run::LaunchPaths,
    executable: &Path,
    token: Option<&provenant_providers::claude::OauthToken>,
) -> Result<run::PreparedLaunch, String> {
    prepare_with(
        registry,
        task,
        prompt,
        paths,
        None,
        |_| canonical(executable),
        token,
    )
}
