//! `workflow` — check reviews and keep receipts and session checkpoints.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "workflow",
    verbs: &[
        Verb {
            path: &["workflow", "receipt", "save"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["workflow", "receipt", "show"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["workflow", "review"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["workflow", "session", "diff"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["workflow", "session", "handoff"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["workflow", "session", "save"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["workflow", "session", "show"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Workflow(provenant_cli::Workflow::Session(
            provenant_cli::workflow::Session::Handoff(args),
        )) => crate::handoff::run(*args, globals, process.workers()),
        Command::Workflow(cmd) => {
            let session_save = matches!(
                cmd,
                provenant_cli::Workflow::Session(provenant_cli::workflow::Session::Save { .. })
            );
            let workspace = if session_save {
                crate::roots::Workspace::SessionSave
            } else {
                crate::roots::Workspace::Project
            };
            let project_warning = if session_save {
                crate::roots::session_project_warning(globals.project.as_deref())
            } else {
                None
            };
            match crate::roots::resolve(globals, workspace, crate::roots::Product::Required) {
                Ok(resolved) => {
                    let state = resolved.roots.state.as_path().to_owned();
                    let home = resolved.home.clone();
                    let project = resolved.project.clone();
                    provenant_cli::workflow::run(
                        cmd,
                        globals,
                        &resolved.roots,
                        &resolved.home,
                        &resolved.project,
                        move || {
                            let mut warnings: Vec<String> = Vec::new();
                            warnings.extend(project_warning);
                            warnings.extend(crate::bounded_session_warning(move || {
                                crate::disk_guard::session_warning(&state, &home, &project)
                            }));
                            warnings
                        },
                    )
                }
                Err(error) => {
                    let outcome = match error {
                        crate::roots::RootError::SessionProjectMissing => Outcome::Refused,
                        _ => Outcome::Error,
                    };
                    crate::fail(
                        inv.leaf,
                        outcome,
                        error.code(),
                        &error.to_string(),
                        crate::REPAIR_ROOT,
                        globals,
                    )
                }
            }
        }
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
