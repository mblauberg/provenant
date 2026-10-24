//! Attempt-owned temporary paths and host-side read-only plan construction.
use crate::{artifacts::subdir, *};
use provenant_confinement::{ConfinedPlan, PlanRequest, ReadOnlyRepo, plan::ReadOnlyPolicy};
use provenant_core::{AttemptId, WorkScope};
use std::{ffi::OsStr, path::PathBuf};

impl RunStore {
    /// The read-only lane's private temporary directory, `LaunchPaths::temporary`.
    /// Provider scratch and build output live below this attempt, never in a
    /// shared system temporary directory. Settlement removes it best-effort.
    pub fn temporary_path(&self, id: AttemptId, call: &Request<'_>) -> Result<PathBuf> {
        let record = self.record(id, call)?;
        let directory = self.attempt_dir(record.run_id, id, false)?;
        subdir(&directory, OsStr::new("tmp"), true)?;
        Ok(self
            .context
            .state_root
            .as_path()
            .join("runs")
            .join(record.run_id.to_string())
            .join("attempts")
            .join(id.to_string())
            .join("tmp"))
    }

    /// The run-plan edge reads policy from host state and resolves all grants
    /// against a host-verified repository before any provider is handed off.
    pub fn read_only_plan(
        &self,
        id: AttemptId,
        repo: &ReadOnlyRepo,
        binding: &provenant_config::ProjectBinding,
        mut request: PlanRequest,
        call: &Request<'_>,
    ) -> Result<ConfinedPlan> {
        if request.scope != WorkScope::Project(self.context.project_id)
            || binding.project_id() != self.context.project_id
        {
            return Err(RunError::Refused);
        }
        request.permissions.scratch.path = self.temporary_path(id, call)?;
        read_only_plan(&self.context.state_root, repo, binding, request)
    }
}

/// Build a host-bound read-only plan for the production preparation edge.
/// Run has already allocated the attempt-private scratch directory in request.
pub fn read_only_plan(
    state: &provenant_core::StateRoot,
    repo: &ReadOnlyRepo,
    binding: &provenant_config::ProjectBinding,
    request: PlanRequest,
) -> Result<ConfinedPlan> {
    if request.scope != WorkScope::Project(binding.project_id()) {
        return Err(RunError::Refused);
    }
    let root = provenant_supervisor::fs::Directory::open_anchor(state.as_path())?;
    let project = match root.open_verified_directory(OsStr::new("projects")) {
        Ok(projects) => {
            match projects.open_verified_directory(OsStr::new(&binding.project_id().to_string())) {
                Ok(project) => Some(project),
                Err(e) if e.is_not_found() => None,
                Err(e) => return Err(e.into()),
            }
        }
        Err(e) if e.is_not_found() => None,
        Err(e) => return Err(e.into()),
    };
    let policy = project
        .as_ref()
        .map(|project| ReadOnlyPolicy::read(project))
        .transpose()
        .map_err(RunError::Confinement)?
        .unwrap_or_default();
    let request = request.with_read_only_repo(repo, binding, &policy)?;
    ConfinedPlan::new(request).map_err(RunError::Plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, input};
    use provenant_confinement::{
        CodexPolicy, ExtensionPolicy, UtilityBudget, UtilityRuntime,
        permissions::{AccessMode, PathGrant, PathScope, PlanPermissions},
    };
    use provenant_core::{CancelToken, RootInputs};
    use provenant_supervisor::{
        exec::{ExecOptions, Executor, SystemExecutor, Termination},
        fs::LocalFs,
    };
    use std::{path::Path, sync::Arc, time::Duration};

    #[test]
    fn run_plan_loads_only_its_bound_projects_policy_and_uses_attempt_temp()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let mut context = context(temp.path())?;
        drop(RunStore::open(&context, &call)?);
        let base = std::fs::canonicalize(temp.path())?;
        let repo_path = base.join("checkout");
        let home = base.join("home");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
        let git = [
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
            "/usr/bin/git",
        ]
        .iter()
        .map(Path::new)
        .find(|p| p.is_file())
        .ok_or("git missing")?;
        let executor = SystemExecutor::new(2)?;
        let argv = [
            git.as_os_str().to_owned(),
            "init".into(),
            "--quiet".into(),
            repo_path.as_os_str().to_owned(),
        ];
        let env = [
            ("HOME".into(), home.as_os_str().to_owned()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ];
        let mut options = ExecOptions::new(&argv, &base, Duration::from_secs(10));
        options.env = Some(&env);
        assert_eq!(executor.execute(options)?.termination, Termination::Exit(0));
        let runtime = UtilityRuntime::new(Arc::new(LocalFs), std::fs::canonicalize(git)?, &home)?;
        let repo = runtime.open_read_repo(
            &executor,
            &repo_path,
            UtilityBudget::bounded(Duration::from_secs(10))?,
            &cancel,
        )?;
        let mut root_input = RootInputs::new(home, repo_path.clone());
        root_input.state_dir = Some(context.state_root.as_path().to_owned());
        let roots = root_input.resolve(&LocalFs)?;
        let binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(repo_path.join(".git"))?,
            &LocalFs,
        )?;
        context.project_id = binding.project_id();
        let store = RunStore::open(&context, &call)?;
        let run = input();
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        std::fs::create_dir_all(repo_path.join(".agent-run/locks"))?;
        let policy_path = context
            .state_root
            .as_path()
            .join("projects")
            .join(binding.project_id().to_string())
            .join("read-only.json");
        let request = || PlanRequest {
            scope: WorkScope::Project(context.project_id),
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
            permissions: PlanPermissions {
                read_only: None,
                mode: AccessMode::ReadOnly,
                scratch: PathGrant {
                    path: base.join("ignored"),
                    scope: PathScope::Subtree,
                },
                private_state: None,
                read_roots: vec![],
                protected_roots: vec![],
                secret_roots: vec![],
                selected_authentication: None,
                include_continuation: false,
                network: vec![],
                capabilities: vec![],
                nested_confinement: false,
            },
        };
        let plan = store.read_only_plan(id, &repo, &binding, request(), &call)?;
        assert!(
            plan.permissions()
                .read_only
                .as_ref()
                .ok_or("read-only grants")?
                .writable_paths()
                .is_empty()
        );
        std::fs::write(&policy_path, r#"{"readOnlyWritable":[".agent-run/locks"]}"#)?;
        let plan = store.read_only_plan(id, &repo, &binding, request(), &call)?;
        assert_eq!(
            plan.permissions()
                .read_only
                .as_ref()
                .ok_or("read-only grants")?
                .writable_paths(),
            [".agent-run/locks"]
        );
        assert_eq!(
            plan.permissions().scratch.path,
            store.temporary_path(id, &call)?
        );
        std::fs::write(&policy_path, r#"{"readOnlyWritable":[".git"]}"#)?;
        assert!(
            store
                .read_only_plan(id, &repo, &binding, request(), &call)
                .is_err()
        );
        Ok(())
    }
}
