//! Process edge for local catalogue reads.
use provenant_cli::{Globals, console::Console};
use provenant_core::CancelToken;
use provenant_records::{CommandResult, Outcome};

pub fn run(command: Console, globals: &Globals) -> u8 {
    let leaf = match &command {
        Console::Projects => "console projects",
        Console::Sessions { .. } => "console sessions",
    };
    let result = (|| -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Optional,
        )?;
        let selected = if globals.project.is_some() {
            use provenant_confinement::{UtilityBudget, UtilityRuntime};
            let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            let runtime = UtilityRuntime::read_only(git)?;
            let executor = provenant_os::exec::SystemExecutor::new(1)?;
            let budget = UtilityBudget::bounded(std::time::Duration::from_secs(5))?;
            let location = runtime
                .project_location(&executor, &resolved.project, budget, &CancelToken::new())?
                .location;
            match provenant_config::lookup_project(&resolved.roots, &location)? {
                Some(binding) => Some(binding.project_id()),
                None => return Err("selected project is not registered".into()),
            }
        } else {
            None
        };
        Ok(provenant_cli::console::read(
            &resolved.roots,
            selected,
            &command,
        )?)
    })();
    match result {
        Ok(value) => {
            let human = format!(
                "{}\n",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
            crate::emit(
                &CommandResult::new(leaf, Outcome::Ok, value),
                globals,
                &human,
                true,
            )
        }
        Err(error) => {
            let (status, code) = match error.downcast_ref::<provenant_cli::console::Error>() {
                Some(
                    provenant_cli::console::Error::Cursor
                    | provenant_cli::console::Error::InvalidSession,
                ) => (Outcome::Invalid, "CATALOGUE_INPUT"),
                _ => (Outcome::Error, "CATALOGUE"),
            };
            crate::fail(leaf, status, code, &error.to_string(), globals)
        }
    }
}
