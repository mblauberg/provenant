//! `reconcile` — settle open attempts and report uncertain custody.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "reconcile",
    verbs: &[Verb {
        path: &["reconcile"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Reconcile(cmd) => crate::run_edge(inv.leaf, globals, |edge| {
            let all = cmd.targets.is_empty() && cmd.abandon.is_none();
            let mut reply = provenant_cli::run::reconcile(cmd, edge);
            if all {
                let issue = match crate::settlement::reconcile_refusal_notices(edge) {
                    Ok(false) => None,
                    Ok(true) => {
                        reply.human.push_str("some refusal notices could not be delivered; inspect their chair bindings and inbox access\n");
                        reply.result.data["refusal_notice_recovery_pending"] =
                            serde_json::json!(true);
                        None
                    }
                    Err(_) => Some(
                        "refusal notice recovery failed; restore inbox access and rerun `provenant reconcile`",
                    ),
                };
                if let Some(message) = issue {
                    reply.human.push_str(&format!("{message}\n"));
                    reply.result.data["refusal_notice_recovery_pending"] = serde_json::json!(true);
                    if reply.result.error.is_none() {
                        reply.result.status = provenant_records::Outcome::Failed;
                        reply.result.error = Some(provenant_records::CommandError::new(
                            "RECONCILE_INCOMPLETE",
                            message,
                            None,
                            "rerun `provenant reconcile`",
                        ));
                    }
                }
            }
            reply
        }),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
