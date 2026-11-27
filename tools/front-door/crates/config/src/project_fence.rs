//! Project-level disposition fences.
//!
//! Retiring a project keeps its registration and settings but adds a fence
//! that removes it from scheduling. `provenant adopt project:ID` clears the
//! fence. The fence lives beside the binding record, so it survives a restart
//! and never unregisters the project.

use crate::ConfigError;
use crate::project_binding::{PROJECTS_DIR, verify_directory};
use provenant_core::{ProjectId, Roots};
use provenant_os::fs::{Directory, PublishMode};
use std::ffi::{OsStr, OsString};

/// A project-level disposition fence. A retired project keeps its registration
/// and settings but is skipped by scheduling until it is adopted again.
const RETIRED_FENCE: &str = "retired";

fn binding_directory(roots: &Roots, project: ProjectId) -> Result<Option<Directory>, ConfigError> {
    let state = Directory::open_anchor(roots.state.as_path())?;
    verify_directory(&state, roots.state.as_path())?;
    let projects = state.open_verified_directory(OsStr::new(PROJECTS_DIR))?;
    let name = OsString::from(project.to_string());
    match projects.open_verified_directory(&name) {
        Ok(directory) => {
            verify_directory(
                &directory,
                &roots.state.as_path().join(PROJECTS_DIR).join(&name),
            )?;
            Ok(Some(directory))
        }
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Fence a registered project out of scheduling without unregistering it.
/// Returns whether a new fence was written; calling twice is idempotent.
pub fn fence_project(roots: &Roots, project: ProjectId) -> Result<bool, ConfigError> {
    let Some(directory) = binding_directory(roots, project)? else {
        return Err(ConfigError::InvalidProjectBinding);
    };
    if directory
        .metadata_at(OsStr::new(RETIRED_FENCE), false)
        .is_ok()
    {
        return Ok(false);
    }
    directory
        .atomic_write(
            OsStr::new(RETIRED_FENCE),
            ".project-retired.",
            b"retired\n",
            PublishMode::NoReplace,
        )?
        .require_durable()?;
    directory.sync()?;
    Ok(true)
}

/// Remove a project's retirement fence. Returns whether a fence was removed.
pub fn unfence_project(roots: &Roots, project: ProjectId) -> Result<bool, ConfigError> {
    let Some(directory) = binding_directory(roots, project)? else {
        return Ok(false);
    };
    match directory.unlink(OsStr::new(RETIRED_FENCE)) {
        Ok(()) => {
            directory.sync()?;
            Ok(true)
        }
        Err(error) if error.is_not_found() => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Whether a registered project is fenced out of scheduling.
pub fn project_is_retired(roots: &Roots, project: ProjectId) -> Result<bool, ConfigError> {
    let Some(directory) = binding_directory(roots, project)? else {
        return Ok(false);
    };
    Ok(directory
        .metadata_at(OsStr::new(RETIRED_FENCE), false)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_binding::{
        ProjectLocation, bind_project, project_binding_dir, registered_projects,
    };
    use std::fs;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn canonical_base(temp: &tempfile::TempDir) -> std::path::PathBuf {
        temp.path().canonicalize().unwrap()
    }

    fn roots(temp: &tempfile::TempDir) -> Roots {
        let inputs = provenant_test_support::test_roots(temp.path()).unwrap();
        inputs
            .resolve(&provenant_test_support::IdentityPathResolver)
            .unwrap()
    }

    #[test]
    fn retire_project_fences_and_adopt_restores() {
        let temp = tempdir();
        let roots = roots(&temp);
        let common = canonical_base(&temp).join("repository/.git");
        fs::create_dir_all(common.join("objects")).unwrap();
        fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
        let binding = bind_project(&roots, ProjectLocation::new(common).unwrap()).unwrap();
        assert!(!project_is_retired(&roots, binding.project_id()).unwrap());

        // Retiring fences the project without unregistering it.
        assert!(fence_project(&roots, binding.project_id()).unwrap());
        assert!(
            !fence_project(&roots, binding.project_id()).unwrap(),
            "idempotent"
        );
        assert!(project_is_retired(&roots, binding.project_id()).unwrap());
        assert!(registered_projects(&roots).unwrap().contains(&binding));
        let fence =
            project_binding_dir(roots.state.as_path(), binding.project_id()).join(RETIRED_FENCE);
        assert!(fence.exists());

        // Adopt (unfence) restores scheduling and is idempotent too.
        assert!(unfence_project(&roots, binding.project_id()).unwrap());
        assert!(!unfence_project(&roots, binding.project_id()).unwrap());
        assert!(!project_is_retired(&roots, binding.project_id()).unwrap());
        assert_eq!(registered_projects(&roots).unwrap(), vec![binding]);
    }
}
