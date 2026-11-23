//! `route` — inspect and edit model routing.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "route",
    verbs: &[
        Verb {
            path: &["route", "exclude", "add"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["route", "exclude", "list"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["route", "exclude", "remove"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["route", "list"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["route", "override", "clear"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["route", "override", "list"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["route", "override", "set"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["route", "pacing"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["route", "remove"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["route", "reset"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["route", "resolve"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["route", "set"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Route(command) => match crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Required,
        ) {
            Ok(resolved) => {
                provenant_cli::route::run(command, globals, &resolved.roots, |result| {
                    crate::overflow::publish(result, globals)
                })
            }
            Err(error) => crate::fail(
                inv.leaf,
                Outcome::Error,
                "ROOT",
                &error.to_string(),
                crate::REPAIR_ROOT,
                globals,
            ),
        },
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
