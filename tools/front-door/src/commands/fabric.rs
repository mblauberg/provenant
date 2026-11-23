//! `fabric` — coordinate messages, tasks, claims and landings between agents.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "fabric",
    verbs: &[
        Verb {
            path: &["fabric", "claim", "acquire"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "claim", "list"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "claim", "release"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "claim", "renew"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "claim", "verify"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "events", "note"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "events", "read"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "identity"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "mail", "mark-read"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "mail", "read"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "mail", "send"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "peer"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["fabric", "peer-agent"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Fabric(provenant_cli::Fabric::Peer { in_process }) => {
            crate::fabric::peer(globals, in_process, process.workers())
        }
        Command::Fabric(provenant_cli::Fabric::PeerAgent) => crate::peer_agent(globals),
        Command::Fabric(provenant_cli::Fabric::Events(provenant_cli::fabric::Events::Read {
            page,
            wait,
        })) => crate::fabric::events(page, wait, globals, process.workers()),
        Command::Fabric(cmd) => match crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Required,
        ) {
            Ok(resolved) => provenant_cli::fabric::run(
                cmd,
                globals,
                &resolved.roots,
                &resolved.home,
                &resolved.project,
            ),
            Err(error) => {
                let status = match error {
                    crate::roots::RootError::Checkout | crate::roots::RootError::State(_) => {
                        Outcome::Invalid
                    }
                    _ => Outcome::Error,
                };
                crate::fail(
                    inv.leaf,
                    status,
                    "ROOT",
                    &error.to_string(),
                    crate::REPAIR_ROOT,
                    globals,
                )
            }
        },
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
