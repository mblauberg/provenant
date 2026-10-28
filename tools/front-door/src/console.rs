//! Process edge for local catalogue reads.
use provenant_cli::{Globals, RenderError, RenderMode, console::Console};
use provenant_core::CancelToken;
use provenant_records::{CommandError, CommandResult, Outcome};
use std::io;

pub fn run(command: Console, globals: &Globals) -> u8 {
    let leaf = match &command {
        Console::Board { .. } => "console board",
        Console::Tree { .. } => "console tree",
        Console::Overview => "console overview",
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
    let result = match result {
        Ok(value) => CommandResult::new(leaf, Outcome::Ok, Some(value)),
        Err(error) => {
            let (status, code) = match error.downcast_ref::<provenant_cli::console::Error>() {
                Some(
                    provenant_cli::console::Error::Cursor
                    | provenant_cli::console::Error::InvalidSession
                    | provenant_cli::console::Error::BoardCursor,
                ) => (Outcome::Invalid, "CATALOGUE_INPUT"),
                _ => (Outcome::Error, "CATALOGUE"),
            };
            let mut result = CommandResult::new(leaf, status, None);
            result.error = Some(CommandError {
                code: code.into(),
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
        result
            .data
            .as_ref()
            .map(provenant_cli::terse)
            .unwrap_or_default()
    };
    render(
        &result,
        globals.json,
        &human,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

// Never publish an overflow artefact, including when rendering errors.
fn render(
    result: &CommandResult<Option<serde_json::Value>>,
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
            let mut failure = CommandResult::new(&result.command, Outcome::Error, ());
            failure.error = Some(CommandError {
                code: if matches!(error, RenderError::TooLarge) {
                    "RESULT_TOO_LARGE"
                } else {
                    "IO"
                }
                .into(),
                message: if matches!(error, RenderError::TooLarge) {
                    if result.command == "console board" {
                        "board exceeds 1 MiB; reduce --limit. If --limit 1 still fails, the row cannot fit this surface; inspect bounded artefacts with `provenant output ATTEMPT`. No artefact was written".into()
                    } else {
                        "catalogue exceeds 1 MiB; no artefact was written".into()
                    }
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

#[cfg(test)]
mod tests {
    #[test]
    fn oversized_single_board_row_explains_recovery() {
        let value =
            serde_json::json!({"lanes":[{"metadata":"x".repeat(provenant_cli::MAX_RESULT_BYTES)}]});
        let result = provenant_records::CommandResult::new(
            "console board",
            provenant_records::Outcome::Ok,
            Some(value),
        );
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            super::render(&result, true, "", &mut stdout, &mut stderr),
            3
        );
        let failure: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(failure["error"]["code"], "RESULT_TOO_LARGE");
        let message = failure["error"]["message"].as_str().unwrap();
        assert!(message.contains("reduce --limit"));
        assert!(message.contains("--limit 1 still fails"));
        assert!(message.contains("provenant output ATTEMPT"));
    }

    #[test]
    fn oversized_human_catalogue_never_publishes() {
        let row = serde_json::json!({"project_id":"x".repeat(32), "common_dir":{"encoding":"utf8","path":"a".repeat(180)}});
        let value = serde_json::json!({"projects":vec![row; 6500], "coverage":{"scope":"local"}});
        let human = provenant_cli::terse(&value);
        assert!(human.len() > provenant_cli::MAX_RESULT_BYTES);
        let result = provenant_records::CommandResult::new(
            "console projects",
            provenant_records::Outcome::Ok,
            Some(value),
        );
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            super::render(&result, false, &human, &mut stdout, &mut stderr),
            3
        );
        assert!(stdout.is_empty());
        assert!(
            String::from_utf8(stderr)
                .unwrap()
                .contains("RESULT_TOO_LARGE")
        );
    }
}
