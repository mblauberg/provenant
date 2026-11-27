//! Parse-only command contracts whose execution arrives in later batch slices.
use crate::commands::{Call, Domain, Verb};
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "contracts",
    verbs: &[
        Verb {
            path: &["retire"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["queue", "hold"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["queue", "resume"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["queue", "prioritise"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["queue", "status"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["queue", "accept"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["queue", "fix"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["queue", "drop"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    crate::commands::not_implemented(inv.leaf)
}
