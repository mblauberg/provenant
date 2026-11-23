//! `telemetry` — report local usage, cost and pipeline experiments.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "telemetry",
    verbs: &[
        Verb {
            path: &["telemetry", "cost"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["telemetry", "experiments", "show"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["telemetry", "ingest"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["telemetry", "mileage"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["telemetry", "prices", "refresh"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["telemetry", "stats"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["telemetry", "trace"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["telemetry", "value"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Telemetry(provenant_cli::Telemetry::Stats(cmd)) => {
            if cmd.usage {
                crate::usage::run(&cmd, globals)
            } else {
                crate::run_edge_emitting(
                    inv.leaf,
                    globals,
                    crate::roots::Workspace::Project,
                    false,
                    |edge| provenant_cli::run::stats(cmd, edge),
                )
            }
        }
        Command::Telemetry(provenant_cli::Telemetry::Trace(cmd)) => {
            crate::run_edge(inv.leaf, globals, |edge| {
                provenant_cli::run::trace(cmd, edge)
            })
        }
        Command::Telemetry(provenant_cli::Telemetry::Prices {
            command: provenant_cli::Prices::Refresh(cmd),
        }) => crate::run_edge(inv.leaf, globals, |edge| {
            provenant_cli::run::prices_refresh(cmd, edge)
        }),
        Command::Telemetry(provenant_cli::Telemetry::Ingest(cmd)) => {
            crate::usage::ingest(&cmd, globals)
        }
        Command::Telemetry(provenant_cli::Telemetry::Mileage(cmd)) => {
            crate::run_edge(inv.leaf, globals, |edge| {
                provenant_cli::mileage::run(cmd, edge)
            })
        }
        Command::Telemetry(provenant_cli::Telemetry::Value(cmd)) => {
            crate::run_edge(inv.leaf, globals, |edge| {
                provenant_cli::telemetry_value::value(cmd, edge)
            })
        }
        Command::Telemetry(provenant_cli::Telemetry::Cost(cmd)) => {
            crate::run_edge(inv.leaf, globals, |edge| {
                provenant_cli::telemetry_value::cost(cmd, edge)
            })
        }
        Command::Telemetry(provenant_cli::Telemetry::Experiments(
            provenant_cli::Experiments::Show(cmd),
        )) => crate::run_edge_workspace(
            inv.leaf,
            globals,
            crate::roots::Workspace::Project,
            |edge| {
                let stats = provenant_cli::Stats {
                    outcomes: true,
                    suggest: false,
                    pipeline: cmd.pipeline,
                    usage: false,
                    since: None,
                    until: None,
                };
                let mut reply = provenant_cli::run::stats(stats, edge);
                if let Some(pipeline) = reply.result.data.get("pipeline") {
                    match provenant_telemetry::experiments::show(pipeline, &cmd.id) {
                        Ok(report) => {
                            reply.result.command = "telemetry experiments show".into();
                            reply.result.data = report;
                            reply.human = format!(
                                "Experiment {}: {} arms; {} comparisons\n",
                                cmd.id,
                                reply.result.data["arms"].as_array().map_or(0, Vec::len),
                                reply.result.data["suggestions"]
                                    .as_array()
                                    .map_or(0, Vec::len),
                            );
                        }
                        Err(message) => {
                            reply.result.status = Outcome::Invalid;
                            reply.result.error = Some(provenant_records::CommandError::new(
                                "EXPERIMENT_REPORT",
                                message,
                                None,
                                "use a non-empty experiment id without ':' and rerun `provenant telemetry experiments show`",
                            ));
                        }
                    }
                }
                reply
            },
        ),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
