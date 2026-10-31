//! This reader must work inside a lane without opening the host state store.
use provenant_cli::Globals;
use provenant_records::{CommandResult, Outcome};

pub fn run(root: u32, globals: &Globals) -> u8 {
    let result = std::env::var_os(provenant_supervisor::process_tree::ENV)
        .ok_or(provenant_supervisor::process_tree::Unavailable)
        .and_then(|path| {
            provenant_supervisor::process_tree::read(std::path::Path::new(&path), root)
        });
    match result {
        Ok(sample) => {
            let human = format!(
                "{} processes sampled at {}\n",
                sample.processes.len(),
                sample.sampled_at
            );
            crate::emit(
                &CommandResult::new("process-tree", Outcome::Ok, Some(sample)),
                globals,
                &human,
                true,
            )
        }
        Err(error) => crate::fail(
            "process-tree",
            Outcome::Unavailable,
            "PROCESS_TREE_UNAVAILABLE",
            &error.to_string(),
            globals,
        ),
    }
}
