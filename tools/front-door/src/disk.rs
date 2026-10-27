//! Compose filesystem inventories without initialising any state owners.
use crate::roots;
use provenant_cli::{Globals, RenderError, RenderMode};
use provenant_confinement::{UtilityBudget, UtilityRuntime, cache::CacheGcPolicy};
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, WaitBudget};
use provenant_os::{clock::SystemClock, disk::DiskReport, exec::SystemExecutor, fs::Directory};
use provenant_records::{CommandError, CommandResult, Outcome};
use provenant_run::{ExecutionContext, Request, RunStore};
use std::{io, path::Path, time::Duration};

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
    let resolved = roots::resolve(
        globals,
        roots::Workspace::Checkout,
        roots::Product::Required,
    )?;
    collect_resolved(&resolved.roots, &resolved.home, &resolved.project)
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
        free_floor_bytes: None,
        protected_owners: Default::default(),
        project_owners: Default::default(),
    };
    let inventory = (|| -> Result<_, Box<dyn std::error::Error>> {
        let git = provenant_os::git::git_executable().ok_or("Git executable unavailable")?;
        let utility = UtilityRuntime::read_only(git)?;
        let executor = SystemExecutor::new(1)?;
        let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
        let repo = utility.open_repo(&executor, project, budget, &cancel)?;
        let registration = (|| -> Result<_, Box<dyn std::error::Error>> {
            let location = provenant_config::ProjectLocation::new(repo.common_git_dir())?;
            let Some(binding) = provenant_config::lookup_project(roots, &location)? else {
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
                    .downcast_ref::<provenant_os::fs::FsError>()
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
                    policy
                        .protected_owners
                        .insert(info.path.clone(), info.writer);
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
        .and_then(|names| names.collect::<provenant_os::fs::Result<Vec<_>>>())
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
    let mut other = provenant_os::disk::DiskEntry {
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
        fn changed_paths(
            before: &BTreeMap<PathBuf, Vec<u8>>,
            after: &BTreeMap<PathBuf, Vec<u8>>,
        ) -> Vec<String> {
            let changed = before
                .iter()
                .filter(|(path, bytes)| after.get(*path) != Some(*bytes))
                .map(|(path, _)| format!("changed or removed {}", path.display()));
            let added = after
                .keys()
                .filter(|path| !before.contains_key(*path))
                .map(|path| format!("added {}", path.display()));
            // Bound the CI message; a fixture tree change rarely needs more.
            changed.chain(added).take(20).collect()
        }
        let rig = crate::worktree_tests::repository()?.without_state()?;
        let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
        inputs.state_dir = Some(rig.state.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let before = snapshot(&rig.root)?;
        let report = collect_resolved(&roots, &rig.home, &rig.project)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!rig.state.exists());
        let changed = changed_paths(&before, &snapshot(&rig.root)?);
        assert!(changed.is_empty(), "report changed files: {changed:?}");
        assert_eq!(report.totals["worktree"].count, 1);
        let main = report
            .entries
            .iter()
            .find(|entry| entry.class == "worktree")
            .ok_or("main worktree")?;
        assert_eq!(main.merged, None);
        assert!(serde_json::to_value(main)?.get("merged").is_none());

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
        for root in [
            &rig.project,
            &rig.project.join(".worktrees/disk-fixture"),
            &rig.state,
        ] {
            fs::create_dir_all(root.join("target"))?;
            fs::write(root.join("target/CACHEDIR.TAG"), "cache")?;
        }
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
        let _binding = provenant_config::bind_project(&roots, location)?;
        let before = snapshot(&rig.root)?;
        let report = collect_resolved(&roots, &rig.home, &rig.project)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let changed = changed_paths(&before, &snapshot(&rig.root)?);
        assert!(changed.is_empty(), "report changed files: {changed:?}");
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
        let main = report
            .entries
            .iter()
            .find(|entry| entry.path == rig.project)
            .ok_or("main worktree")?;
        assert_eq!(main.merged, None);
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
        let entries = json["data"]["entries"].as_array().ok_or("entries")?;
        let targets: Vec<_> = entries
            .iter()
            .filter(|entry| entry["class"] == "host_target")
            .collect();
        assert_eq!(targets.len(), 3);
        for target in targets {
            let path = Path::new(target["path"].as_str().ok_or("target path")?);
            assert!(target.get("reclaim").is_none());
            if path == rig.state.join("target") {
                assert!(target.get("owner").is_none());
                assert!(target.get("writer").is_none());
            } else {
                let worktree = entries
                    .iter()
                    .find(|entry| {
                        entry["class"] == "worktree"
                            && entry["path"].as_str().map(Path::new) == path.parent()
                    })
                    .ok_or("target's worktree")?;
                assert_eq!(target["owner"], worktree["path"]);
                assert!(matches!(
                    worktree["writer"].as_str(),
                    Some("live" | "free" | "unknown")
                ));
                assert_eq!(target["writer"], worktree["writer"]);
            }
        }
        assert!(json["data"]["volume"]["available_bytes"].is_u64());
        for key in ["method", "volume", "entries", "totals", "errors"] {
            assert!(!json["data"][key].is_null(), "missing {key}");
        }
        Ok(())
    }

    #[test]
    fn disk_report_for_many_runs_stays_small_in_both_render_modes()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::{collections::BTreeMap, fs, os::unix::fs::MetadataExt};
        const RUNS: u64 = 100;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let state = root.join("state");
        let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("project"));
        inputs.state_dir = Some(state.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        fs::create_dir_all(state.join("children"))?;
        // Mostly empty files and directories exercise physical classification
        // without SQLite inserts or provider setup. Only the first and last
        // runs need payload blocks to check that folding preserves bytes.
        fs::write(state.join("run.sqlite3"), [])?;
        fs::write(state.join("config.toml"), [])?;
        let mut expected = BTreeMap::<&str, u64>::new();
        for index in 0..RUNS {
            let contents: &[u8] = if index == 0 || index == RUNS - 1 {
                &[7; 4096]
            } else {
                &[]
            };
            let attempt = state.join(format!("runs/{index}/attempts/a"));
            fs::create_dir_all(&attempt)?;
            for (name, class) in [
                ("result.md", "other_state"),
                ("stdout.log", "attempt_retained"),
            ] {
                let part = attempt.join(name);
                fs::write(&part, contents)?;
                *expected.entry(class).or_default() += fs::symlink_metadata(part)?.blocks() * 512;
            }
            let child = state.join(format!("children/{index}"));
            fs::create_dir(&child)?;
            if !contents.is_empty() {
                let payload = child.join("payload");
                fs::write(&payload, contents)?;
                *expected.entry("child_store").or_default() +=
                    fs::symlink_metadata(payload)?.blocks() * 512;
            }
            *expected.entry("child_store").or_default() +=
                fs::symlink_metadata(child)?.blocks() * 512;
        }
        for name in ["run.sqlite3", "config.toml"] {
            *expected.entry("other_state").or_default() +=
                fs::symlink_metadata(state.join(name))?.blocks() * 512;
        }
        let mut report = DiskReport::new(state.clone(), 0);
        RunStore::disk_usage_unindexed(&roots.state, &mut report)?;
        other_state(&state, &mut report);
        // Capture all original byte observations before publication compacts
        // rows, including APFS private sizes and newest-write timestamps.
        let mut before = BTreeMap::<String, (u64, u64, u64, i128)>::new();
        for entry in &report.entries {
            let total = before.entry(entry.class.clone()).or_default();
            total.0 += 1;
            total.1 += entry.allocated_bytes;
            total.2 += entry.unshared_bytes;
            total.3 = total.3.max(entry.newest_write_ns);
        }
        report.finish();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.entries.len(), 3);
        for entry in &report.entries {
            let count = RUNS + u64::from(entry.class == "other_state");
            assert_eq!(entry.count, Some(count));
            let total = &report.totals[&entry.class];
            assert!(total.allocated_bytes > 0);
            assert_eq!(total.count, count);
            assert_eq!(total.allocated_bytes, expected[entry.class.as_str()]);
            assert_eq!(
                (
                    total.count,
                    total.allocated_bytes,
                    total.unshared_bytes,
                    entry.newest_write_ns
                ),
                before[&entry.class]
            );
        }
        let value = serde_json::to_value(&report)?;
        report.finish();
        assert_eq!(serde_json::to_value(&report)?, value);
        let result = CommandResult::new("disk report", Outcome::Ok, Some(report));
        let human = provenant_cli::terse(&value);
        for json in [true, false] {
            let mut output = Vec::new();
            let mut errors = Vec::new();
            assert_eq!(render(&result, json, &human, &mut output, &mut errors), 0);
            assert!(errors.is_empty());
            assert!(!output.is_empty());
            assert!(output.len() < RUNS as usize * 16, "{} bytes", output.len());
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
