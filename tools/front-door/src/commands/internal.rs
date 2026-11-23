//! `internal` — hidden process-edge commands the guardian and tick invoke.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "internal",
    verbs: &[
        Verb {
            path: &["internal", "event-listen"],
            stability: Stability::Internal,
            run,
        },
        Verb {
            path: &["internal", "lane-cleanup"],
            stability: Stability::Internal,
            run,
        },
        Verb {
            path: &["internal", "launch"],
            stability: Stability::Internal,
            run,
        },
        Verb {
            path: &["internal", "provider-updates"],
            stability: Stability::Internal,
            run,
        },
        Verb {
            path: &["internal", "settle"],
            stability: Stability::Internal,
            run,
        },
        Verb {
            path: &["internal", "supervise"],
            stability: Stability::Internal,
            run,
        },
        Verb {
            path: &["internal", "tick-gc"],
            stability: Stability::Internal,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Internal(provenant_cli::Internal::Supervise { descriptors }) => {
            crate::supervise(&descriptors, globals, process)
        }
        Command::Internal(provenant_cli::Internal::Settle { request }) => {
            // Recovery commits terminal state without a live guardian, so it
            // drains the settlement reactions itself.
            let result = (|| -> Result<(), String> {
                let parsed = provenant_run::settlement::ResumeRequest::parse(&request)
                    .map_err(|error| error.to_string())?;
                let mut inputs = provenant_core::RootInputs::new(
                    parsed.roots().home.clone(),
                    parsed.roots().checkout.clone(),
                );
                inputs.state_dir = Some(parsed.roots().state_root.clone());
                let roots = inputs
                    .resolve(&provenant_os::fs::SystemPathResolver)
                    .map_err(|error| error.to_string())?;
                let subject = crate::settlement::Subject {
                    home: parsed.roots().home.clone(),
                    checkout: parsed.roots().checkout.clone(),
                    state_root: parsed.roots().state_root.clone(),
                    project: parsed.project(),
                    actor: parsed.actor(),
                    attempt: parsed.attempt(),
                };
                let registry = provenant_cli::run::Registry {
                    roots: Some(&roots),
                    ..Default::default()
                };
                provenant_run::settlement::resume(&request, &registry)
                    .map_err(|error| error.to_string())?;
                crate::fabric::settlement_reactions(&subject, None, process.workers())
                    .map_err(|error| error.to_string())
            })();
            match result {
                Ok(()) => 0,
                Err(error) => crate::fail(
                    inv.leaf,
                    Outcome::Error,
                    "SETTLEMENT",
                    &error,
                    "rerun `provenant reconcile` to finish settlement",
                    globals,
                ),
            }
        }
        Command::Internal(provenant_cli::Internal::LaneCleanup { request }) => {
            match crate::fabric::gc_cleanup::run(&request, process.workers()) {
                Ok(()) => 0,
                Err(error) => crate::fail(
                    inv.leaf,
                    Outcome::Error,
                    "LANE_CLEANUP",
                    &error.to_string(),
                    "rerun lane cleanup after the writer releases custody",
                    globals,
                ),
            }
        }
        Command::Internal(provenant_cli::Internal::TickGc) => {
            crate::tick::gc_worker(globals, process.workers())
        }
        Command::Internal(provenant_cli::Internal::EventListen { repository }) => {
            crate::event_delivery::listen(&repository, globals)
        }
        Command::Internal(provenant_cli::Internal::Launch {
            attempt,
            provider_lease_wait,
        }) => {
            let receipt_globals = provenant_cli::Globals {
                json: true,
                full: globals.full,
                state_dir: globals.state_dir.clone(),
                project: globals.project.clone(),
            };
            crate::run_edge(inv.leaf, &receipt_globals, |edge| {
                provenant_cli::run::launch_worker(attempt, provider_lease_wait, edge)
            })
        }
        Command::Internal(provenant_cli::Internal::ProviderUpdates) => {
            match crate::roots::resolve(
                globals,
                crate::roots::Workspace::Project,
                crate::roots::Product::Required,
            ) {
                Ok(resolved) => match provenant_cli::provider_updates::housekeep(
                    &resolved.roots,
                    &resolved.home,
                    &resolved.project,
                ) {
                    Ok(()) => 0,
                    Err(error) => crate::fail(
                        inv.leaf,
                        Outcome::Error,
                        "PROVIDER_UPDATE",
                        &error,
                        "rerun `provenant tick` after the reported cause is resolved",
                        globals,
                    ),
                },
                Err(error) => crate::fail(
                    inv.leaf,
                    Outcome::Error,
                    "ROOT",
                    &error.to_string(),
                    crate::REPAIR_ROOT,
                    globals,
                ),
            }
        }
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
