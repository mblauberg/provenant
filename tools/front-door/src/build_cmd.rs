use crate::fail;
use provenant_cli::{Build, Globals};
use provenant_records::{CommandResult, Outcome};

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
            let mut report =
                provenant_run::build::report::aggregate(&rows, &table.build, limit, ram, |_, _| {
                    None
                });
            report
                .warnings
                .push("churn selection is unavailable; unknown rows cannot pass".into());
            let mut human = format!(
                "build report: {} rows, {} lane rows\n",
                report.rows, report.lane_rows
            );
            for (name, verdict) in &report.criteria {
                human.push_str(&format!("{name}: {verdict:?}\n"));
            }
            human.push_str("churn selection is unavailable; unknown rows cannot pass\n");
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
                    let dir = provenant_os::fs::Directory::open_anchor(&paths.root)
                        .map_err(|e| e.to_string())?;
                    let _lock = dir
                        .try_lock(
                            std::ffi::OsStr::new("lock"),
                            provenant_os::fs::LockMode::Exclusive,
                        )
                        .map_err(|e| e.to_string())?;
                    let mut record = provenant_run::build::slots::SlotRecord::read(&paths.record)
                        .map_err(|e| e.to_string())?
                        .ok_or("missing slot record")?;
                    if full {
                        provenant_run::build::hygiene::full_reset(&paths)
                            .map_err(|e| e.to_string())?;
                        record.prepare_digest = None;
                    } else {
                        provenant_run::build::hygiene::light_reset(&paths)
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
        Build::Maintain => fail(
            "build maintain",
            Outcome::Unavailable,
            "BUILD_NOT_IMPLEMENTED",
            "build maintenance is not yet integrated",
            globals,
        ),
        Build::Status => unreachable!(),
    }
}
