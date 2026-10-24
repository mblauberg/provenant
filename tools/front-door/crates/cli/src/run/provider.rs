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

    let executor = provenant_supervisor::exec::SystemExecutor::new(1).map_err(|e| e.to_string())?;
    let settings = provenant_config::read_with_domains(
        roots,
        &provenant_supervisor::fs::LocalFs,
        &crate::domains::SECTIONS,
    )
    .map_err(|e| e.to_string())?
    .domains
    .accounts;
    let mut entries: Vec<_> = settings.entries.into_values().collect();
    if !entries.iter().any(|e| e.adapter == "codex") {
        let directory = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        if let Ok(directory) = canonical(&directory)
            && let Ok(executable) = resolve_executable("codex")
            && let Ok(key) = provenant_providers::accounts::detect(
                &executor,
                "codex",
                &executable,
                &home,
                &directory,
                &paths.scratch,
            )
        {
            entries.push(provenant_providers::accounts::Registration {
                adapter: "codex".into(),
                label: "default".into(),
                key,
                directory,
            });
        }
    }
    // This profile exposes no stored OpenCode login: every free request uses
    // the same keyless allowance on this host, rather than an inferred user.
    if resolve_executable("opencode").is_ok() {
        entries.push(provenant_providers::accounts::Registration {
            adapter: "opencode".into(),
            label: "keyless".into(),
            key: "keyless".into(),
            directory: paths.provider_state.clone(),
        });
    }
    let timestamp = jiff::Timestamp::now().as_second();
    let catalogue = provenant_routing::Catalogue::bundled().map_err(|e| e.to_string())?;
    let mut availability = Vec::new();
    let mut reasons = Vec::new();
    for entry in &entries {
        match observe_account(
            entry,
            &executor,
            &home,
            &paths.scratch,
            catalogue.revision(),
            timestamp,
        ) {
            Ok(record) => availability.push(record),
            Err(error) => reasons.push(format!(
                "{} account {}: {error}",
                entry.adapter, entry.label
            )),
        }
    }
    let digest = task.authority_digest.to_string();
    let mut access = Vec::new();
    for adapter in ["codex", "claude", "opencode"] {
        if confinement_plan(
            project_id,
            &cwd,
            &home,
            &paths.scratch,
            &paths.provider_state,
            writer.as_ref(),
            adapter,
        )
        .is_ok()
        {
            access.push(provenant_routing::AccessContext {
                adapter,
                disclosure_ref: &digest,
                confinement_ref: &digest,
            });
        }
    }
    let requested_adapter = match &task.route {
        run::RequestedRoute::Model { model, .. } => {
            model.split_once('/').map(|(adapter, _)| adapter)
        }
        run::RequestedRoute::Named { .. } => None,
    };
    let selected = task
        .account
        .as_deref()
        .map(|label| {
            let mut matching = entries.iter().filter(|e| {
                (e.label == label || e.key == label)
                    && requested_adapter.is_none_or(|adapter| e.adapter == adapter)
            });
            let entry = matching.next().ok_or_else(|| {
                format!("account {label} is not registered for the requested route")
            })?;
            if matching.any(|other| other.key != entry.key) {
                return Err(format!(
                    "account alias {label} is ambiguous; use its exact registered key"
                ));
            }
            Ok(entry.key.as_str())
        })
        .transpose()?;
    let host = provenant_config::bind_host(roots, &provenant_supervisor::fs::LocalFs)
        .map_err(|e| e.to_string())?
        .to_string();
    let resolved = crate::route::resolve_dispatch(
        &route_request(task)?,
        roots,
        selected,
        &availability,
        &access,
        &host,
    )?;
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
        if let provenant_records::RouteReadiness::Unavailable { reason } = &route.readiness {
            reasons.push(format!("{label}: {reason:?}"));
            continue;
        }
        if let Err(error) = crate::route::check_dispatch_candidate(roots, &route) {
            reasons.push(format!("{label}: {error}"));
            continue;
        }
        if !matches!(adapter.id(), "codex" | "claude" | "opencode") {
            reasons.push(format!("{label}: provider launch is not supported here"));
            continue;
        }
        let entry = entries
            .iter()
            .find(|e| e.adapter == route.adapter && Some(&e.key) == route.account_key.as_ref());
        let account = entry
            .filter(|entry| matches!(entry.adapter.as_str(), "claude" | "codex"))
            .map(|entry| entry.registered())
            .transpose()
            .map_err(|e| e.to_string())?;
        if matches!(adapter.id(), "codex" | "claude") && account.is_none() {
            reasons.push(format!(
                "{label}: account is not registered; use accounts register"
            ));
            continue;
        }
        let executable = match resolve_executable(adapter.id()) {
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
        let plan = match confinement_plan(
            project_id,
            &cwd,
            &home,
            scratch,
            private_state,
            writer.as_ref(),
            adapter.id(),
        ) {
            Ok(plan) => plan,
            Err(error) => {
                reasons.push(format!("{label}: {error}"));
                continue;
            }
        };
        let token = if adapter.id() == "claude" {
            let account = account.as_ref().ok_or("Claude account is not registered")?;
            let login = provenant_providers::claude::Login {
                executable: &executable,
                home: &home,
                user: &user,
                config_dir: account.directory(),
                scratch,
            };
            let authority = provenant_providers::claude::CredentialAuthority::for_dispatch(
                paths.run_id.to_string(),
                &login,
                paths.deadline_unix_ms,
            )
            .map_err(|e| e.to_string())?;
            let record = serde_json::to_vec(&authority).map_err(|e| e.to_string())?;
            let parent =
                Directory::open_anchor(private_state.parent().ok_or("attempt directory missing")?)
                    .map_err(|e| e.to_string())?;
            let publication = parent
                .atomic_write(
                    std::ffi::OsStr::new("credential-authority.json"),
                    ".authority-",
                    &record,
                    provenant_supervisor::fs::PublishMode::Replace,
                )
                .map_err(|e| e.to_string())?;
            if !matches!(publication, provenant_supervisor::fs::Publication::Durable) {
                return Err("credential authority durability unconfirmed".into());
            }
            let now = || u64::try_from(jiff::Timestamp::now().as_millisecond()).unwrap_or(0);
            match provenant_providers::claude::refresh_for_account_authorised(
                &executor,
                &login,
                account,
                &now,
                paths.deadline_unix_ms,
                &authority,
            ) {
                Ok(token) => Some(token),
                Err(error) => {
                    reasons.push(format!("{label}: account {}: {error}; sign in again with CLAUDE_CONFIG_DIR={} claude auth login", entry.map_or("unknown", |e| e.label.as_str()), account.directory().display()));
                    continue;
                }
            }
        } else {
            None
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
            account: account.as_ref(),
            token: token.as_ref(),
            cwd: &cwd,
            nonce,
        }) {
            Ok(spec) => spec,
            Err(error) => {
                reasons.push(format!("{label}: {error}"));
                continue;
            }
        };
        let transcript_account = account
            .as_ref()
            .filter(|_| adapter.id() == "claude")
            .map(|account| account.directory());
        if let Err(error) = apply_setup(&spec.setup, &[scratch, private_state], transcript_account)
        {
            reasons.push(format!("{label}: setup failed: {error}"));
            continue;
        }
        let provider = RouteRecord {
            provider: Some(route.adapter.clone()),
            model: Some(format!("{}/{}", route.adapter, route.model)),
            effort: route.effort,
            account_label: entry.map(|e| e.label.clone()),
            account_key: route.account_key,
            ..Default::default()
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

fn observe_account(
    entry: &provenant_providers::accounts::Registration,
    executor: &provenant_supervisor::exec::SystemExecutor,
    home: &Path,
    scratch: &Path,
    catalogue_revision: &str,
    timestamp: i64,
) -> Result<provenant_providers::AvailabilityRecord, String> {
    let executable = resolve_executable(&entry.adapter)?;
    let directory = canonical(&entry.directory)?;
    if directory != entry.directory
        || (entry.adapter == "claude" && directory != home.join(format!(".claude-{}", entry.label)))
    {
        return Err(format!(
            "{} account {} has no fixed canonical identity directory",
            entry.adapter, entry.label
        ));
    }
    let detected = if entry.adapter == "opencode" {
        entry.key.clone()
    } else {
        provenant_providers::accounts::detect(
            executor,
            &entry.adapter,
            &executable,
            home,
            &directory,
            scratch,
        )
        .map_err(|e| {
            format!(
                "{} account {}: {e}; re-register the signed-in identity",
                entry.adapter, entry.label
            )
        })?
    };
    if detected != entry.key {
        return Err(format!(
            "{} account {} changed identity; re-register it",
            entry.adapter, entry.label
        ));
    }
    let argv = [executable.clone().into_os_string(), "--version".into()];
    let mut options = provenant_supervisor::exec::ExecOptions::new(
        &argv,
        scratch,
        std::time::Duration::from_secs(4),
    );
    let env = [
        ("HOME".into(), home.as_os_str().to_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    options.env = Some(&env);
    options.stdout = provenant_supervisor::exec::StreamPolicy::Capture { limit: 128 };
    options.stderr = provenant_supervisor::exec::StreamPolicy::Discard;
    use provenant_supervisor::exec::Executor;
    let out = executor.execute(options).map_err(|e| e.to_string())?;
    if out.termination != provenant_supervisor::exec::Termination::Exit(0)
        || out.timed_out
        || out.stdout.truncated
    {
        return Err("provider version is unavailable".into());
    }
    let version = String::from_utf8(out.stdout.bytes)
        .map_err(|_| "invalid provider version")?
        .trim()
        .to_owned();
    if version.is_empty() {
        return Err("provider version is unavailable".into());
    }
    Ok(provenant_providers::AvailabilityRecord {
        schema_version: 1,
        adapter: entry.adapter.clone(),
        executable: executable.to_string_lossy().into_owned(),
        version,
        fixture_ref: format!("activated/{}/fresh", entry.adapter),
        catalogue_revision: catalogue_revision.into(),
        account_key: Some(entry.key.clone()),
        observed_models: Vec::new(),
        last_preflight_error: None,
        observed_unix_seconds: timestamp,
        expires_unix_seconds: timestamp.saturating_add(300),
    })
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
    let mut provider_route = route.clone();
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
    adapter: &str,
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
            selected_authentication: (adapter == "codex").then(|| "login".into()),
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

pub(crate) fn resolve_executable(name: &str) -> Result<PathBuf, String> {
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

/// Setup stays below run-owned roots, except Claude's selected account projects.
/// Only directory creation gets that exception; links remain run-owned.
/// Traversal opens each component without following links.
fn apply_setup(
    steps: &[SetupStep],
    roots: &[&Path],
    claude_account: Option<&Path>,
) -> Result<(), String> {
    for step in steps {
        match step {
            SetupStep::CreateDir(path) => {
                let transcript_root =
                    claude_account.filter(|directory| path.starts_with(directory.join("projects")));
                let (parent, name) = match transcript_root {
                    Some(directory) => setup_parent(&[directory], path, true)?,
                    None => setup_parent(roots, path, true)?,
                };
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
    confinement_plan(
        project,
        cwd,
        home,
        &paths.scratch,
        &paths.provider_state,
        Some(writer),
        "codex",
    )
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
                None,
            )?;
            assert!(scratch.join("cargo-home/nested").is_dir());
            apply_setup(
                &[SetupStep::CreateDir(outside.join("projects/session"))],
                &roots,
                Some(&outside),
            )?;
            assert!(outside.join("projects/session").is_dir());
            assert!(
                apply_setup(
                    &[SetupStep::CreateDir(outside.join("credentials/new"))],
                    &roots,
                    Some(&outside)
                )
                .is_err()
            );
            assert!(
                apply_setup(
                    &[SetupStep::Symlink {
                        link: outside.join("projects/link"),
                        target: state.clone()
                    }],
                    &roots,
                    Some(&outside)
                )
                .is_err()
            );
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
                assert!(apply_setup(&[step], &roots, None).is_err());
            }
            assert!(!outside.join("x").exists());
            Ok(())
        })();
        std::fs::remove_dir_all(&base)?;
        result
    }
}
