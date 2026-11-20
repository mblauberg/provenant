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
    let name = match command {
        Build::Status => "build status",
        Build::Report { .. } => "build report",
        Build::Reset { .. } => "build reset",
        Build::Maintain => "build maintain",
    };
    fail(
        name,
        Outcome::Unavailable,
        "BUILD_NOT_IMPLEMENTED",
        "build command implementation is not available yet",
        globals,
    )
}
