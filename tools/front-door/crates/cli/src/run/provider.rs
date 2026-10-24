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
        // A read-only lane's scratch is its private temporary directory, which
        // terminal commit and withdrawal remove; writer scratch keeps settlement.
        let scratch = if writer.is_some() {
            &paths.scratch
        } else {
            &paths.temporary
        };
        let private_state = &paths.provider_state;
        let plan = match confinement_request(
            project_id,
            &cwd,
            &home,
            scratch,
            private_state,
            writer.as_ref(),
        )
        .and_then(|request| match writer.as_ref() {
            Some(_) => ConfinedPlan::new(request).map_err(|error| error.to_string()),
            None => read_only_plan(roots, project_id, &cwd, request),
        }) {
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
        if let Err(error) = apply_setup(&spec.setup, &[scratch, private_state]) {
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

fn confinement_request(
    project_id: ProjectId,
    cwd: &Path,
    home: &Path,
    scratch: &Path,
    private_state: &Path,
    writer: Option<&run::WriterLaunch>,
) -> Result<PlanRequest, String> {
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
    Ok(PlanRequest {
        scope: WorkScope::Project(project_id),
        permissions: PlanPermissions {
            read_only: None,
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
}

/// Verify the selected repository on the host and match the persisted dispatch
/// project before deriving any Git read authority from that binding.
fn read_only_plan(
    roots: &provenant_core::Roots,
    project: ProjectId,
    cwd: &Path,
    request: PlanRequest,
) -> Result<ConfinedPlan, String> {
    use provenant_confinement::{UtilityBudget, UtilityRuntime};
    use provenant_supervisor::{exec::SystemExecutor, fs::LocalFs};
    let git = [
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
        "/usr/bin/git",
        "/bin/git",
    ]
    .into_iter()
    .map(Path::new)
    .find(|path| provenant_supervisor::sys::is_executable(path))
    .ok_or("Git executable unavailable")?;
    let utility = UtilityRuntime::new(
        std::sync::Arc::new(LocalFs),
        canonical(git)?,
        roots.state.as_path().join("tmp/git-home"),
    )
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
            Err(error) => return Err(error.to_string()),
        }
    }
    let location = provenant_config::ProjectLocation::new(
        repository
            .as_ref()
            .map_or(cwd, |repo| repo.common_git_dir()),
    )
    .map_err(|error| error.to_string())?;
    let binding = provenant_config::lookup_project(roots, &location, &LocalFs)
        .map_err(|error| error.to_string())?
        .ok_or("dispatch project binding missing")?;
    if binding.project_id() != project {
        return Err("repository does not match dispatch project binding".into());
    }
    match repository {
        Some(repo) => run::read_only_plan(&roots.state, &repo, &binding, request)
            .map_err(|error| error.to_string()),
        // Non-Git workspaces have no Git metadata or repository exceptions.
        None => ConfinedPlan::new(request).map_err(|error| error.to_string()),
    }
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

/// Setup stays strictly below the run-owned scratch or private state roots.
/// Traversal opens each component without following links.
fn apply_setup(steps: &[SetupStep], roots: &[&Path]) -> Result<(), String> {
    for step in steps {
        match step {
            SetupStep::CreateDir(path) => {
                let (parent, name) = setup_parent(roots, path, true)?;
                match parent.mkdir(name, 0o700) {
                    Ok(()) => parent.sync().map_err(|error| error.to_string())?,
                    // Fallback candidates share one attempt's paths.
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(error.to_string()),
                }
                parent
                    .open_verified_directory(name)
                    .map_err(|error| error.to_string())?;
            }
            SetupStep::Symlink { link, target } => {
                let (parent, name) = setup_parent(roots, link, false)?;
                parent
                    .symlink(target, name)
                    .map_err(|error| error.to_string())?;
                parent.sync().map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

fn setup_parent<'a>(
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
        if create {
            match directory.mkdir(component, 0o700) {
                Ok(()) => directory.sync().map_err(|error| error.to_string())?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        directory = directory
            .open_verified_directory(component)
            .map_err(|error| error.to_string())?;
    }
    Ok((directory, name))
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
    let request = confinement_request(
        project,
        cwd,
        home,
        &paths.scratch,
        &paths.provider_state,
        Some(writer),
    )?;
    ConfinedPlan::new(request).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_stays_below_run_owned_roots_without_following_links()
    -> Result<(), Box<dyn std::error::Error>> {
        let base = std::env::temp_dir().join(format!("provenant-setup-{}", std::process::id()));
        std::fs::create_dir(&base)?;
        let base = canonical(&base)?;
        let (scratch, state, outside) = (base.join("tmp"), base.join("state"), base.join("out"));
        for path in [&scratch, &state, &outside] {
            std::fs::create_dir(path)?;
        }
        let roots = [scratch.as_path(), state.as_path()];
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            apply_setup(
                &[
                    SetupStep::CreateDir(scratch.join("cargo-home/nested")),
                    SetupStep::CreateDir(scratch.join("cargo-home/nested")),
                    SetupStep::Symlink {
                        link: scratch.join("cargo-home/registry"),
                        target: outside.clone(),
                    },
                ],
                &roots,
            )?;
            assert!(scratch.join("cargo-home/nested").is_dir());
            std::os::unix::fs::symlink(&outside, scratch.join("escape"))?;
            for step in [
                SetupStep::CreateDir(outside.join("x")),
                SetupStep::CreateDir(scratch.join("../out/x")),
                SetupStep::CreateDir(scratch.clone()),
                SetupStep::CreateDir(scratch.join("escape/x")),
                SetupStep::Symlink {
                    link: scratch.join("cargo-home/registry/x"),
                    target: outside.clone(),
                },
            ] {
                assert!(apply_setup(&[step], &roots).is_err());
            }
            assert!(!outside.join("x").exists());
            Ok(())
        })();
        std::fs::remove_dir_all(&base)?;
        result
    }
}
