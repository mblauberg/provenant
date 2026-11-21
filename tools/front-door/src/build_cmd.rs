use crate::fail;
use provenant_cli::{Build, Globals};
use provenant_config::PortTable;
use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

/// Cached expected-rebuild set for one `(prev, snapshot)` pair. `None` records
/// a genuine selection failure so a later report does not retry forever.
#[derive(Serialize, Deserialize)]
struct ChurnCache {
    schema: u32,
    expected: Option<Vec<String>>,
}

fn churn_cache_path(state: &Path, prev: &str, snap: &str) -> PathBuf {
    state
        .join("build/report-cache")
        .join(format!("{prev}-{snap}.json"))
}

fn git_command(args: &[String]) -> bool {
    let Some(git) = provenant_os::git::git_executable() else {
        return false;
    };
    let mut argv = vec![git.into_os_string()];
    argv.extend(args.iter().map(OsString::from));
    let Ok(executor) = SystemExecutor::new(1) else {
        return false;
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let options = ExecOptions::diagnostic(&argv, &cwd, Duration::from_secs(120));
    executor
        .execute(options)
        .is_ok_and(|outcome| outcome.termination == Termination::Exit(0))
}

/// A temporary detached worktree of the project's bare store, removed on drop.
struct ReportCheckout {
    store: PathBuf,
    temp: PathBuf,
    work: PathBuf,
}
impl Drop for ReportCheckout {
    fn drop(&mut self) {
        let store = self.store.to_string_lossy().into_owned();
        let work = self.work.to_string_lossy().into_owned();
        let _ = git_command(&[
            format!("--git-dir={store}"),
            "worktree".into(),
            "remove".into(),
            "--force".into(),
            work,
        ]);
        // Only remove the directory this process created, under the temp root.
        if self.temp.starts_with(std::env::temp_dir())
            && self
                .temp
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("provenant-report-"))
        {
            let _ = std::fs::remove_dir_all(&self.temp);
        }
    }
}

fn report_checkout(store: &Path, commit: &str) -> Option<ReportCheckout> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = std::env::temp_dir().join(format!(
        "provenant-report-{}-{}-{}",
        std::process::id(),
        provenant_run::build::ledger::now_unix_ms(),
        unique
    ));
    std::fs::create_dir(&temp).ok()?;
    let work = temp.join("work");
    let store_arg = format!("--git-dir={}", store.display());
    let commit = commit.to_owned();
    if git_command(&[
        store_arg,
        "worktree".into(),
        "add".into(),
        "--detach".into(),
        "--force".into(),
        work.to_string_lossy().into_owned(),
        commit,
    ]) {
        Some(ReportCheckout {
            store: store.to_path_buf(),
            temp,
            work,
        })
    } else {
        let _ = std::fs::remove_dir_all(&temp);
        None
    }
}

fn package_name(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let document: toml_edit::DocumentMut = text.parse().ok()?;
    document
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(|name| name.as_str())
        .map(str::to_owned)
}

/// Packages that are rebuilt regardless of the diff: the configured root
/// package, packages whose build script declares no rerun-if directive, and
/// Cargo's build-script fingerprint unit.
fn always_rebuild(root: &Path, workspace: &Path) -> BTreeSet<String> {
    let mut set = BTreeSet::from(["build-script-build".to_owned()]);
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = pending.pop() {
        if depth > 8 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut manifest = false;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if matches!(name.as_str(), "target" | ".git" | "node_modules") {
                continue;
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push((entry.path(), depth + 1));
            } else if name == "Cargo.toml" {
                manifest = true;
            }
        }
        if !manifest {
            continue;
        }
        let Some(name) = package_name(&directory.join("Cargo.toml")) else {
            continue;
        };
        let is_root = directory == workspace;
        let unbounded = std::fs::read_to_string(directory.join("build.rs"))
            .map(|script| {
                !script.contains("cargo:rerun-if-changed")
                    && !script.contains("cargo:rerun-if-env-changed")
            })
            .unwrap_or(false);
        if is_root || unbounded {
            set.insert(name);
        }
    }
    set
}

fn compute_expected(
    store: &Path,
    prev: &str,
    snap: &str,
    key: &str,
    ports: &PortTable,
    state: &Path,
    now_ms: u64,
) -> Option<BTreeSet<String>> {
    let checkout = report_checkout(store, snap)?;
    let workspace = checkout.work.join(&ports.verify.workspace);
    let select_state = state.join("build/report-cache/select-state");
    let state_file = select_state.join("select").join(format!("{key}.json"));
    if !state_file.exists() {
        std::fs::create_dir_all(state_file.parent()?).ok()?;
        let bytes = serde_json::json!({
            "version": 1,
            "runs_since_full": 0,
            "last_full_unix_millis": now_ms,
        })
        .to_string();
        std::fs::write(&state_file, bytes).ok()?;
    }
    let policy = provenant_select::Policy {
        global_paths: ports.verify.affected.global_paths.clone(),
        full_every: u32::MAX,
        full_after: Duration::from_secs(100 * 365 * 86_400),
    };
    let now = UNIX_EPOCH + Duration::from_millis(now_ms);
    let selection =
        provenant_select::select(&workspace, prev, snap, &policy, &select_state, key, now).ok()?;
    let mut expected: BTreeSet<String> = selection.packages.into_iter().collect();
    expected.extend(always_rebuild(&checkout.work, &workspace));
    Some(expected)
}

/// Build CLI handlers are introduced before their owning report and hygiene slices.
pub fn run(command: Build, globals: &Globals) -> u8 {
    if matches!(command, Build::Status) {
        let resolved = match crate::roots::resolve(
            globals,
            crate::roots::Workspace::Home,
            crate::roots::Product::Optional,
        ) {
            Ok(resolved) => resolved,
            Err(error) => {
                return fail(
                    "build status",
                    Outcome::Invalid,
                    "ROOT",
                    &error.to_string(),
                    globals,
                );
            }
        };
        let rows =
            provenant_run::build::hygiene::status(&resolved.home, resolved.roots.state.as_path());
        let store_bytes =
            provenant_run::build::hygiene::store_bytes(resolved.roots.state.as_path());
        let mut human = String::from("PROJECT SLOT HEALTH ALLOCATED_BYTES LAST_JOB\n");
        for row in &rows {
            let last = row
                .last_job
                .as_ref()
                .map_or_else(|| "-".into(), |job| job.finished_unix_ms.to_string());
            human.push_str(&format!(
                "{} {} {} {} {}\n",
                row.project_key, row.index, row.health, row.allocated_bytes, last
            ));
        }
        human.push_str(&format!("store allocated bytes: {store_bytes}\n"));
        let data = serde_json::json!({"slots": rows, "store_allocated_bytes": store_bytes});
        return crate::emit(
            &CommandResult::new("build status", Outcome::Ok, data),
            globals,
            &human,
            true,
        );
    }
    let resolved = match crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Optional,
    ) {
        Ok(value) => value,
        Err(error) => {
            return fail(
                "build",
                Outcome::Invalid,
                "ROOT",
                &error.to_string(),
                globals,
            );
        }
    };
    let state = resolved.roots.state.as_path();
    let key = provenant_config::project_key_for(&resolved.project)
        .unwrap_or_else(|| provenant_config::project_key(&resolved.project));
    match command {
        Build::Report { since } => {
            let table = match provenant_config::PortTable::read_for_inspection(
                Some(&resolved.project),
                state,
            ) {
                Ok(table) => table,
                Err(error) => {
                    return fail(
                        "build report",
                        Outcome::Error,
                        "CONFIG",
                        &error.to_string(),
                        globals,
                    );
                }
            };
            let limit = provenant_config::ResolvedAdmissionClass::resolve(
                "build",
                state,
                Some(&resolved.project),
                Some(&key),
            )
            .slots;
            let cutoff = provenant_run::build::ledger::now_unix_ms()
                .saturating_sub(since.unwrap_or(u64::MAX).saturating_mul(1000));
            let root = provenant_run::build::paths::build_root(state);
            let mut rows = Vec::new();
            if let Ok(files) = std::fs::read_dir(root) {
                use std::io::BufRead;
                for file in files.flatten() {
                    let name = file.file_name().to_string_lossy().into_owned();
                    if name != "jobs.jsonl"
                        && !(name.starts_with("jobs-") && name.ends_with(".jsonl"))
                    {
                        continue;
                    }
                    if let Ok(input) = std::fs::File::open(file.path()) {
                        for line in std::io::BufReader::new(input).lines().map_while(Result::ok) {
                            if let Ok(row) =
                                serde_json::from_str::<provenant_run::build::JobOutcome>(&line)
                                && row.finished_unix_ms >= cutoff
                                && (globals.project.is_none() || row.project_key == key)
                            {
                                rows.push(row);
                            }
                        }
                    }
                }
            }
            let ram = provenant_os::sys::resources::memory()
                .ok()
                .map(|memory| memory.total_bytes);
            let now_ms = provenant_run::build::ledger::now_unix_ms();
            let store = provenant_run::build::paths::store_path(state, &key);
            let cache_root = state.join("build/report-cache");
            let _ = std::fs::create_dir_all(&cache_root);
            let pairs: BTreeSet<(String, String)> = rows
                .iter()
                .filter_map(|row| match (&row.prev_snapshot, &row.snapshot) {
                    (Some(prev), Some(snap)) => Some((prev.clone(), snap.clone())),
                    _ => None,
                })
                .collect();
            let mut expected_map: BTreeMap<(String, String), Option<BTreeSet<String>>> =
                BTreeMap::new();
            let mut unknown = 0;
            for (prev, snap) in pairs {
                let cache = churn_cache_path(state, &prev, &snap);
                let expected = if let Ok(bytes) = std::fs::read(&cache) {
                    serde_json::from_slice::<ChurnCache>(&bytes)
                        .ok()
                        .and_then(|cached| cached.expected.map(|set| set.into_iter().collect()))
                } else {
                    let value = compute_expected(&store, &prev, &snap, &key, &table, state, now_ms);
                    let cached = ChurnCache {
                        schema: 1,
                        expected: value.as_ref().map(|set| set.iter().cloned().collect()),
                    };
                    if let Ok(bytes) = serde_json::to_vec(&cached) {
                        let _ = std::fs::write(&cache, bytes);
                    }
                    value
                };
                if expected.is_none() {
                    unknown += 1;
                }
                expected_map.insert((prev, snap), expected);
            }
            let mut report = provenant_run::build::report::aggregate(
                &rows,
                &table.build,
                limit,
                ram,
                |prev, snap| {
                    expected_map
                        .get(&(prev.to_owned(), snap.to_owned()))
                        .cloned()
                        .unwrap_or(None)
                },
            );
            if unknown > 0 {
                report.warnings.push(format!(
                    "churn selection unavailable for {unknown} snapshot pair(s); those rows cannot pass"
                ));
            }
            let mut human = format!(
                "build report: {} rows, {} lane rows\n",
                report.rows, report.lane_rows
            );
            for (name, verdict) in &report.criteria {
                human.push_str(&format!("{name}: {verdict:?}\n"));
            }
            if unknown > 0 {
                human.push_str(&format!(
                    "churn selection unavailable for {unknown} snapshot pair(s); those rows cannot pass\n"
                ));
            }
            crate::emit(
                &CommandResult::new(
                    "build report",
                    Outcome::Ok,
                    serde_json::to_value(report).unwrap_or_default(),
                ),
                globals,
                &human,
                true,
            )
        }
        Build::Reset { slot, full } => {
            let mut warnings = Vec::new();
            let mut count = 0;
            for row in provenant_run::build::hygiene::status(&resolved.home, state)
                .into_iter()
                .filter(|r| r.project_key == key && slot.is_none_or(|index| index == r.index))
            {
                let paths =
                    provenant_run::build::slots::SlotPaths::new(&resolved.home, &key, row.index);
                let result = (|| -> Result<(), String> {
                    // An explicit reset is destructive too: take the same
                    // custody guard collection uses and hold it through the
                    // reset and the record write.
                    let custody = provenant_run::build::slots::SlotCustody::acquire(&paths)
                        .map_err(|e| e.to_string())?;
                    let mut record = provenant_run::build::slots::SlotRecord::read(&paths.record)
                        .map_err(|e| e.to_string())?
                        .ok_or("missing slot record")?;
                    if full {
                        provenant_run::build::hygiene::full_reset(&paths, &custody)
                            .map_err(|e| e.to_string())?;
                        record.prepare_digest = None;
                    } else {
                        provenant_run::build::hygiene::light_reset(&paths, &custody)
                            .map_err(|e| e.to_string())?;
                    }
                    record.health = Default::default();
                    record.write(&paths.record).map_err(|e| e.to_string())?;
                    Ok(())
                })();
                match result {
                    Ok(()) => count += 1,
                    Err(error) => warnings.push(format!("slot {}: {error}", row.index)),
                }
            }
            let human = format!("reset {count} build slots\n{}", warnings.join("\n"));
            crate::emit(
                &CommandResult::new(
                    "build reset",
                    Outcome::Ok,
                    serde_json::json!({"reset": count, "warnings": warnings}),
                ),
                globals,
                &human,
                true,
            )
        }
        Build::Maintain => {
            let now =
                UNIX_EPOCH + Duration::from_millis(provenant_run::build::ledger::now_unix_ms());
            let outcome = provenant_run::build::hygiene::maintain(
                state,
                &resolved.home,
                &resolved.project,
                now,
            );
            let mut human = format!(
                "build maintenance: {} (canary: {})\n",
                if outcome.ran { "ran" } else { "skipped" },
                outcome.canary
            );
            if let Some(warning) = &outcome.warning {
                human.push_str(&format!("warning: {warning}\n"));
            }
            crate::emit(
                &CommandResult::new(
                    "build maintain",
                    Outcome::Ok,
                    serde_json::to_value(&outcome).unwrap_or_default(),
                ),
                globals,
                &human,
                true,
            )
        }
        Build::Status => unreachable!(),
    }
}
