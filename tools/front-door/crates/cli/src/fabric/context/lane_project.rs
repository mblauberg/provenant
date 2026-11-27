//! A lane's project comes from the binding its launch declared, never from
//! its working directory: a pinned read view is an unregistered standalone
//! repository whose object store is an alternate, so discovery from it is
//! refused, and the lane sandbox does not grant the live source. Outside a
//! lane, contexts keep discovering their project from the working directory.
use super::{
    Context, active_harness_client, decide_timeout_source, has_git_common_layout, private_dir,
    prompt_limit_source,
};
use crate::fabric::EdgeError;
use provenant_core::{
    ProjectId, Roots,
    consumer_env::{LANE_PROJECT, PRIMARY_CHECKOUT},
};
use provenant_fabric_runtime::identity::{CrossHostProjectKey, Identity, IdentityError};
use provenant_os::exec::SystemExecutor;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

/// How a lane context obtains this host's identity.
#[derive(Clone, Copy)]
pub(super) enum LaneHost {
    /// Read-only contexts never create state.
    Lookup,
    /// Mutation-capable contexts may publish the host identity.
    Bind,
}

/// The project the lane's launch declared in `PROVENANT_LANE_PROJECT`. A
/// published binding is read by id. A transient project has no record; its
/// binding is the transient one of the declared primary checkout, and only
/// when that binding has the declared id.
pub(super) fn declared_binding(
    roots: &Roots,
) -> Result<provenant_config::ProjectBinding, EdgeError> {
    let state = roots.state.as_path();
    let value = std::env::var_os(LANE_PROJECT).ok_or_else(|| {
        refusal(format!(
            "{LANE_PROJECT} is not set in this lane, so no project binding under {} can be \
             chosen; relaunch the lane with a current provenant so its launch declares the \
             project",
            state.display()
        ))
    })?;
    let project = value
        .to_str()
        .and_then(|value| value.parse::<ProjectId>().ok())
        .ok_or_else(|| {
            refusal(format!(
                "{LANE_PROJECT}={} is not a project id, so no project binding under {} can \
                 be chosen; relaunch the lane with a current provenant so its launch declares \
                 the project",
                value.to_string_lossy(),
                state.display()
            ))
        })?;
    let path = provenant_config::project_binding_dir(state, project);
    let unreadable = |error: provenant_config::ConfigError| {
        refusal(format!(
            "{LANE_PROJECT}={project} names a binding at {} that cannot be read: {error}; \
             repair the host project binding (run provenant in the project on the host), \
             then relaunch the lane",
            path.display()
        ))
    };
    if let Some(binding) =
        provenant_config::lookup_project_by_id(state, project).map_err(unreadable)?
    {
        return Ok(binding);
    }
    if let Some(location) = declared_transient_location(roots)
        && let Some(binding) = provenant_config::lookup_project(roots, &location)
            .map_err(unreadable)?
            .filter(|binding| binding.project_id() == project)
    {
        return Ok(binding);
    }
    Err(refusal(format!(
        "{LANE_PROJECT}={project} has no project binding at {}; bind the project on the \
         host (run provenant in it), then relaunch the lane",
        path.display()
    )))
}

/// The transient location the launch's primary checkout names: its Git
/// directory when it holds one, else the directory itself.
fn declared_transient_location(roots: &Roots) -> Option<provenant_config::ProjectLocation> {
    let primary = PathBuf::from(std::env::var_os(PRIMARY_CHECKOUT)?);
    let git = primary.join(".git");
    let common = if git.is_dir() { git } else { primary };
    if !provenant_config::transient_location(roots, &common) {
        return None;
    }
    provenant_config::ProjectLocation::new(common).ok()
}

fn refusal(message: String) -> EdgeError {
    EdgeError::LaneProject(message)
}

/// Project and configuration roots of a declared binding, without Git: the
/// primary checkout hint, else the directory holding the common Git
/// directory. A bare repository has no checkout and stays host-only; a
/// non-Git project is its own root.
pub(super) fn binding_roots(
    binding: &provenant_config::ProjectBinding,
) -> (PathBuf, Option<PathBuf>, bool) {
    let common = binding.common_dir();
    if !has_git_common_layout(common) {
        return (common.to_path_buf(), Some(common.to_path_buf()), false);
    }
    let checkout = binding
        .primary_checkout_hint()
        .map(Path::to_path_buf)
        .or_else(|| {
            common
                .parent()
                .filter(|_| common.file_name() == Some(OsStr::new(".git")))
                .map(Path::to_path_buf)
        });
    match checkout {
        Some(root) => (root.clone(), Some(root), true),
        None => (common.to_path_buf(), None, true),
    }
}

impl Context {
    /// The context of a lane, bound to the project its launch declared.
    pub(super) fn open_in_lane(
        roots: Roots,
        home: &Path,
        cwd: &Path,
        host: LaneHost,
    ) -> Result<Self, EdgeError> {
        let resolved = crate::config_reads::read_for(&roots, "fabric context")?;
        let graph_prompt_limit_source = prompt_limit_source(&resolved.origins);
        let graph_decide_timeout_source = decide_timeout_source(&resolved.origins);
        let local_observation = crate::wait::observation_settings(&resolved);
        let config = resolved.domains.fabric;
        let host = match host {
            LaneHost::Lookup => provenant_config::lookup_host(&roots)?.ok_or(EdgeError::Invalid)?,
            LaneHost::Bind => {
                private_dir(roots.state.as_path(), roots.state.as_path())?;
                provenant_config::bind_host(&roots)?
            }
        };
        let binding = declared_binding(&roots)?;
        let (project_root, project_config_root, repository) = binding_roots(&binding);
        let port_table = provenant_config::PortTable::read_for(
            project_config_root.as_deref(),
            roots.state.as_path(),
        )?;
        let key = if repository {
            match CrossHostProjectKey::from_home(home, binding.common_dir()) {
                Ok(key) => Some(key),
                Err(IdentityError::OutsideHome) => None,
                Err(error) => return Err(error.into()),
            }
        } else {
            None
        };
        let identity = Identity::bind(
            &binding,
            host,
            cwd,
            &config,
            std::env::var_os(provenant_core::consumer_env::AGENT_LABEL).as_deref(),
            active_harness_client(),
            key,
        )?;
        Ok(Self {
            inspection_only: false,
            roots,
            identity,
            config,
            projection: super::projection_from_knobs(&resolved.knobs),
            local_observation,
            port_table,
            project_root,
            graph_prompt_limit_source,
            graph_decide_timeout_source,
            in_lane: true,
            executor: SystemExecutor::new(1)?,
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
        })
    }
}
