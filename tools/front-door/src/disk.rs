//! Compose filesystem inventories without initialising any state owners.
use crate::roots;
use provenant_cli::{Globals, RenderError, RenderMode};
use provenant_confinement::{UtilityBudget, UtilityRuntime, permissions::CacheGcPolicy};
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, WaitBudget};
use provenant_records::{CommandError, CommandResult, Outcome};
use provenant_run::{ExecutionContext, Request, RunStore};
use provenant_supervisor::{
    clock::SystemClock,
    disk::DiskReport,
    exec::SystemExecutor,
    fs::{Directory, LocalFs},
};
use std::{io, path::Path, sync::Arc, time::Duration};

pub fn run(globals: &Globals) -> u8 {
    let mut result = match collect(globals) {
        Ok(report) => CommandResult::new("disk report", Outcome::Ok, Some(report)),
        Err(error) => {
            let outcome = if matches!(
                error.downcast_ref::<roots::RootError>(),
                Some(roots::RootError::Checkout | roots::RootError::State(_))
            ) {
                Outcome::Invalid
            } else {
                Outcome::Error
            };
            let mut result = CommandResult::new("disk report", outcome, None);
            result.error = Some(CommandError {
                code: "DISK".into(),
                message: error.to_string(),
                cause: None,
                repair: None,
            });
            result
        }
    };
    let human = if globals.json {
        String::new()
    } else {
        match result.data.as_ref().map(serde_json::to_value).transpose() {
            Ok(Some(value)) => provenant_cli::terse(&value),
            Ok(None) => String::new(),
            Err(error) => {
                result.status = Outcome::Error;
                result.data = None;
                result.error = Some(CommandError {
                    code: "ENCODE".into(),
                    message: error.to_string(),
                    cause: None,
                    repair: None,
                });
                String::new()
            }
        }
    };
    render(
        &result,
        globals.json,
        &human,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

// Deliberately use the bounded renderer directly: overflow publication creates
// state and is forbidden for this command, even on its failure paths.
fn render(
    result: &CommandResult<Option<DiskReport>>,
    json: bool,
    human: &str,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> u8 {
    let mode = if json {
        RenderMode::Json
    } else {
        RenderMode::Human {
            text: human,
            read_only: true,
        }
    };
    let rendered = if !json && result.error.is_some() {
        provenant_cli::render(result, mode, stderr)
    } else {
        provenant_cli::render(result, mode, stdout)
    };
    match rendered {
        Ok(()) => provenant_cli::exit_status(&result.status),
        Err(error) => {
            let mut failure = CommandResult::new("disk report", Outcome::Error, ());
            failure.error = Some(CommandError {
                code: if matches!(error, RenderError::TooLarge) {
                    "RESULT_TOO_LARGE"
                } else {
                    "IO"
                }
                .into(),
                message: if matches!(error, RenderError::TooLarge) {
                    "disk report exceeds 1 MiB; no artefact was written".into()
                } else {
                    error.to_string()
                },
                cause: None,
                repair: None,
            });
            if json {
                let _ = provenant_cli::render(&failure, RenderMode::Json, stdout);
            } else {
                let _ = provenant_cli::render(
                    &failure,
                    RenderMode::Human {
                        text: "",
                        read_only: true,
                    },
                    stderr,
                );
            }
            3
        }
    }
}

fn collect(globals: &Globals) -> Result<DiskReport, Box<dyn std::error::Error>> {
    let assets = roots::product_root()?;
    let home = roots::home()?;
    let project = roots::selected_project(globals.project.as_deref(), true)?;
    let roots = roots::roots(globals, &assets, &project)?;
    collect_resolved(&roots, &home, &project)
}

fn collect_resolved(
    roots: &provenant_core::Roots,
    home: &Path,
    project: &Path,
) -> Result<DiskReport, Box<dyn std::error::Error>> {
    let state = roots.state.as_path();
    // A missing state directory is an empty inventory, not an invitation to
    // create it. Report available space from its nearest existing ancestor.
    let volume_path = state
        .ancestors()
        .find(|path| path.is_dir())
        .ok_or("missing volume")?;
    let volume = Directory::open_anchor(volume_path)?;
    let mut report = DiskReport::new(volume_path.into(), volume.available_bytes()?);
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let mut policy = CacheGcPolicy {
        idle_before_ns: Some(
            i128::from(clock.wall_now()?.as_micros()) * 1000 - 7 * 86_400 * 1_000_000_000_i128,
        ),
        dry_run: true,
        protected_owners: Default::default(),
        project_owners: Default::default(),
    };
    let inventory = (|| -> Result<_, Box<dyn std::error::Error>> {
        let git = roots::git_binary().ok_or("Git executable unavailable")?;
        let utility = UtilityRuntime::read_only(Arc::new(LocalFs), git)?;
        let executor = SystemExecutor::new(1)?;
        let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
        let repo = utility.open_repo(&executor, project, budget, &cancel)?;
        let registration = (|| -> Result<_, Box<dyn std::error::Error>> {
            let location = provenant_config::ProjectLocation::new(repo.common_git_dir())?;
            let Some(binding) = provenant_config::lookup_project(roots, &location, &LocalFs)?
            else {
                return Ok(None);
            };
            let project_state = Directory::open_anchor(state)?
                .open_verified_directory(std::ffi::OsStr::new("worktrees"))?
                .open_verified_directory(std::ffi::OsStr::new(&binding.project_id().to_string()))?;
            Ok(Some((binding, project_state)))
        })();
        let registration = match registration {
            Ok(registration) => registration,
            Err(error)
                if error
                    .downcast_ref::<provenant_supervisor::fs::FsError>()
                    .is_some_and(|error| error.is_not_found()) =>
            {
                None
            }
            Err(error) => {
                report.error(state, error);
                None
            }
        };
        Ok(provenant_confinement::disk::worktrees(
            &repo,
            &utility,
            &executor,
            budget,
            &cancel,
            &mut report,
            registration
                .as_ref()
                .map(|(binding, directory)| (binding, directory)),
        )?)
    })();
    match inventory {
        Ok(worktrees) => {
            for info in worktrees {
                if info.writer != provenant_confinement::worktrees::WriterState::Free {
                    policy.protected_owners.insert(info.path.clone());
                }
                policy.project_owners.insert(info.path);
            }
        }
        Err(error) => report.error(project, error),
    }
    provenant_confinement::disk::lane_caches(home, &policy, &mut report);
    // Physical inventory spans all projects. These inert IDs are never used
    // for a scoped record read, identity publication or write.
    let context = ExecutionContext {
        state_root: roots.state.clone(),
        project_id: ProjectId::from_bytes([0; 16]),
        actor_id: ActorId::from_bytes([0; 16]),
        read_prompts: false,
    };
    let request = Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 5)?,
        cancel: &cancel,
    };
    let indexed = match RunStore::open_disk_read_only(&context, &request) {
        Ok(Some(store)) => match store.disk_usage(&mut report) {
            Ok(()) => true,
            Err(error) => {
                report.error(state, error);
                false
            }
        },
        Ok(None) => false,
        Err(error) => {
            report.error(state, error);
            false
        }
    };
    if !indexed && let Err(error) = RunStore::disk_usage_unindexed(&roots.state, &mut report) {
        report.error(state, error);
    }
    other_state(state, &mut report);
    report.finish();
    Ok(report)
}

fn other_state(path: &Path, report: &mut DiskReport) {
    let directory = match Directory::open_anchor(path) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return,
        Err(error) => {
            report.error(path, error);
            return;
        }
    };
    let names = match directory
        .entries()
        .and_then(|names| names.collect::<provenant_supervisor::fs::Result<Vec<_>>>())
    {
        Ok(mut names) => {
            names.sort();
            names
        }
        Err(error) => {
            report.error(path, error);
            return;
        }
    };
    let mut other = provenant_supervisor::disk::DiskEntry {
        class: "other_state".into(),
        path: path.into(),
        ..Default::default()
    };
    let mut observed = false;
    for name in names {
        if name != "runs"
            && name != "children"
            && !name
                .to_str()
                .is_some_and(|name| name.starts_with("run.sqlite3"))
        {
            report.measure_into_at(&mut other, &directory, &name, &path.join(&name));
            observed = true;
        }
    }
    if observed {
        report.add_rollup(other);
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)] // The worktree driver includes this module without libtest.
    use super::*;

    #[test]
    fn disk_report_inventories_worktrees_and_unindexed_state_without_writes()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::{collections::BTreeMap, fs, path::PathBuf};
        fn snapshot(path: &Path) -> io::Result<BTreeMap<PathBuf, Vec<u8>>> {
            let mut result = BTreeMap::new();
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let path = entry.path();
                if entry.file_type()?.is_dir() {
                    result.insert(path.clone(), Vec::new());
                    result.extend(snapshot(&path)?);
                } else if entry.file_type()?.is_file() {
                    result.insert(path.clone(), fs::read(&path)?);
                }
            }
            Ok(result)
        }
        let rig = crate::worktree_tests::repository()?;
        let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
        inputs.state_dir = Some(rig.state.clone());
        let roots = inputs.resolve(&LocalFs)?;
        let before = snapshot(&rig.root)?;
        let report = collect_resolved(&roots, &rig.home, &rig.project)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!rig.state.exists());
        assert!(snapshot(&rig.root)? == before, "report changed files");
        assert_eq!(report.totals["worktree"].count, 1);

        fs::create_dir_all(rig.project.join(".worktrees"))?;
        crate::worktree_tests::git(
            &rig,
            &[
                "worktree",
                "add",
                "-b",
                "disk-fixture",
                ".worktrees/disk-fixture",
            ],
        )?;
        fs::write(
            rig.project.join(".worktrees/disk-fixture/payload"),
            vec![1; 8192],
        )?;
        fs::create_dir_all(rig.state.join("runs/run/attempts/attempt/tmp"))?;
        fs::create_dir_all(rig.state.join("runs/run/attempts/attempt/scratch"))?;
        fs::write(
            rig.state.join("runs/run/attempts/attempt/tmp/keep"),
            "unindexed",
        )?;
        fs::write(rig.state.join("config.toml"), "schema_version = 1\n")?;
        for index in 0..50 {
            fs::write(rig.state.join(format!("metadata-{index}")), "state")?;
        }
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
        // A project identity need not have any worktree registry yet.
        let location = provenant_config::ProjectLocation::new(rig.project.join(".git"))?;
        let _binding = provenant_config::bind_project(&roots, location, &LocalFs)?;
        let before = snapshot(&rig.root)?;
        let report = collect_resolved(&roots, &rig.home, &rig.project)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(snapshot(&rig.root)? == before, "report changed files");
        assert_eq!(report.totals["worktree"].count, 2);
        assert_eq!(report.totals["attempt_tmp"].count, 1);
        assert_eq!(report.totals["other_state"].count, 1);
        let linked = report
            .entries
            .iter()
            .find(|e| e.path.ends_with(".worktrees/disk-fixture"))
            .ok_or("linked worktree")?;
        assert!(linked.allocated_bytes >= 8192);
        assert_eq!(linked.merged, Some(true));
        assert!(
            report
                .entries
                .iter()
                .filter(|e| e.class == "attempt_tmp")
                .all(|e| e.leak.is_none())
        );
        let envelope = CommandResult::new("disk report", Outcome::Ok, Some(report));
        let mut output = Vec::new();
        assert_eq!(render(&envelope, true, "", &mut output, &mut Vec::new()), 0);
        let json: serde_json::Value = serde_json::from_slice(&output)?;
        assert_eq!(json["command"], "disk report");
        assert_eq!(json["data"]["schema_version"], 1);
        assert!(json["data"]["volume"]["available_bytes"].is_u64());
        for key in ["method", "volume", "entries", "totals", "errors"] {
            assert!(!json["data"][key].is_null(), "missing {key}");
        }
        Ok(())
    }
    #[test]
    fn disk_overflow_returns_envelope_without_publishing() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut report = DiskReport::new("/fixture".into(), 0);
        report.error("/fixture", "x".repeat(provenant_cli::MAX_RESULT_BYTES));
        let result = CommandResult::new("disk report", Outcome::Ok, Some(report));
        let mut output = Vec::new();
        assert_eq!(render(&result, true, "", &mut output, &mut Vec::new()), 3);
        let value: serde_json::Value = serde_json::from_slice(&output)?;
        assert_eq!(value["command"], "disk report");
        assert_eq!(value["error"]["code"], "RESULT_TOO_LARGE");
        assert!(value["data"].is_null());
        Ok(())
    }
}
