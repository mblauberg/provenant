//! Provider preparation at the CLI composition edge.
use super::{Registry, route_request};
use provenant_confinement::permissions::{
    AccessMode, PathGrant, PathScope, PlanPermissions, WorktreeWriteAuthority,
};
use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
use provenant_core::{ProjectId, WorkScope};
use provenant_providers::{PrepareInput, Prompt, REGISTRY, SetupStep, Stdin};
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
    if resume.is_some() {
        return Err(
            "continuation needs a provider session; providers do not expose sessions yet".into(),
        );
    }
    let roots = registry.roots.ok_or("routing context is unavailable")?;
    let home = canonical(registry.home.ok_or("home directory is unavailable")?)?;
    let state_root = registry.state_root.ok_or("state root is unavailable")?;
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
    for route in candidates(resolved) {
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
        if adapter.id() == "claude" {
            reasons.push(format!(
                "{label}: Claude credential authority is not available in this revision"
            ));
            continue;
        }
        if adapter.id() != "codex" {
            reasons.push(format!("{label}: provider launch is not supported here"));
            continue;
        }
        let executable = match resolve_executable("codex") {
            Ok(path) => path,
            Err(error) => {
                reasons.push(format!("{label}: {error}"));
                continue;
            }
        };
        let provider_route = match adapter_route(&route) {
            Ok(route) => route,
            Err(error) => {
                reasons.push(format!("{label}: {error}"));
                continue;
            }
        };
        let scratch = &paths.scratch;
        let private_state = &paths.provider_state;
        let plan = match confinement_plan(
            project_id,
            &cwd,
            &home,
            scratch,
            private_state,
            writer.as_ref(),
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
            route: &provider_route,
            confined: &plan,
            prompt: &prompt,
            continuation: None,
            grant_digest: &grant_digest,
            executable: &executable,
            home: &home,
            user: &user,
            account: None,
            token: None,
            cwd: &cwd,
            nonce,
        }) {
            Ok(spec) => spec,
            Err(error) => {
                reasons.push(format!("{label}: {error}"));
                continue;
            }
        };
        if let Err(error) = apply_setup(&spec.setup, private_state) {
            reasons.push(format!("{label}: setup failed: {error}"));
            continue;
        }
        let provider = RouteRecord {
            provider: Some(route.adapter),
            model: Some(route.model),
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
                home: home.clone(),
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

fn adapter_route(route: &ResolvedRoute) -> Result<ResolvedRoute, String> {
    let prefix = format!("{}/", route.adapter);
    let model = route
        .model
        .strip_prefix(&prefix)
        .ok_or("resolved model does not match its provider")?;
    let mut provider_route = route.clone();
    provider_route.model = model.to_owned();
    provider_route.alternatives = Default::default();
    Ok(provider_route)
}

fn confinement_plan(
    project_id: ProjectId,
    cwd: &Path,
    home: &Path,
    scratch: &Path,
    private_state: &Path,
    writer: Option<&run::WriterLaunch>,
) -> Result<ConfinedPlan, String> {
    let mode = match writer {
        Some(writer) => AccessMode::WorktreeWrite(Box::new(
            WorktreeWriteAuthority::for_worktree(&writer.worktree, &writer.branch, home)
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
            selected_authentication: Some("login".into()),
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

fn apply_setup(steps: &[SetupStep], private_state: &Path) -> Result<(), String> {
    for step in steps {
        match step {
            SetupStep::CreateDir(_) => {
                return Err("provider requested an unsupported setup directory".into());
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

/// Fixture access to the production plan builder; installed builds omit it.
#[cfg(feature = "fixture")]
pub fn writer_fixture_plan(
    project: ProjectId,
    cwd: &Path,
    home: &Path,
    paths: &run::LaunchPaths,
    writer: &run::WriterLaunch,
) -> Result<ConfinedPlan, String> {
    confinement_plan(
        project,
        cwd,
        home,
        &paths.scratch,
        &paths.provider_state,
        Some(writer),
    )
}
