//! One owner per command domain: each module declares a `DOMAIN` whose verbs
//! are the leaves of the grammar it owns, and the body `run` moved there from
//! the old dispatch match.
//!
//! The value types live in `provenant_cli` because `usage_verbs()` and
//! `external_contract()` derive from the same table and the binary depends on
//! that crate, not the reverse. This module owns `DOMAINS` for the binary and
//! installs it once at the process edge; `run()` looks a parsed command up by
//! its space-joined path and calls its verb.
//!
//! Spec: docs/specs/rust-workspace.md#one-owner-per-command-domain
//!
//! The registry contract returns `CommandError` by value; it is the type the
//! result envelope already carries, and boxing it would allocate on every
//! refusal to satisfy a size heuristic. One scope-level allow keeps the
//! contracted signature across every domain.
#![allow(clippy::result_large_err)]
mod accounts;
mod adapter;
mod admit;
mod build;
mod chair;
mod check;
mod ci;
mod cloud;
mod config;
mod console;
mod custody;
mod disk;
mod edit;
mod events;
mod fabric;
mod gc;
mod help;
mod hook;
mod hosts;
mod inbox;
mod init;
mod install;
mod internal;
mod land;
mod lane;
mod native;
mod notes;
mod output;
mod pr;
mod process_tree;
mod quota;
mod reconcile;
mod result;
mod retry;
mod root;
mod route;
mod run;
mod status;
mod steer;
mod stop;
mod telemetry;
mod templates;
mod tick;
mod trace;
mod tracker;
mod verify;
mod wait;
mod workflow;
mod worktree;

pub use provenant_cli::{Call, Domain, Verb};

use provenant_records::{CommandError, Outcome};

/// Every command domain, one module per top-level grammar command.
pub static DOMAINS: &[&Domain] = &[
    &accounts::DOMAIN,
    &adapter::DOMAIN,
    &admit::DOMAIN,
    &build::DOMAIN,
    &chair::DOMAIN,
    &check::DOMAIN,
    &ci::DOMAIN,
    &cloud::DOMAIN,
    &config::DOMAIN,
    &console::DOMAIN,
    &custody::DOMAIN,
    &disk::DOMAIN,
    &edit::DOMAIN,
    &events::DOMAIN,
    &fabric::DOMAIN,
    &gc::DOMAIN,
    &help::DOMAIN,
    &hook::DOMAIN,
    &hosts::DOMAIN,
    &inbox::DOMAIN,
    &init::DOMAIN,
    &install::DOMAIN,
    &internal::DOMAIN,
    &land::DOMAIN,
    &lane::DOMAIN,
    &native::DOMAIN,
    &notes::DOMAIN,
    &output::DOMAIN,
    &pr::DOMAIN,
    &process_tree::DOMAIN,
    &quota::DOMAIN,
    &reconcile::DOMAIN,
    &result::DOMAIN,
    &retry::DOMAIN,
    &root::DOMAIN,
    &route::DOMAIN,
    &run::DOMAIN,
    &status::DOMAIN,
    &steer::DOMAIN,
    &stop::DOMAIN,
    &telemetry::DOMAIN,
    &templates::DOMAIN,
    &tick::DOMAIN,
    &trace::DOMAIN,
    &tracker::DOMAIN,
    &verify::DOMAIN,
    &wait::DOMAIN,
    &workflow::DOMAIN,
    &worktree::DOMAIN,
];

/// Installs the registry at the process edge. `usage_verbs()` and the consumer
/// contract read it; a second call in a test process is ignored.
pub fn install() {
    provenant_cli::install_domains(DOMAINS);
}

/// The verb registered for a parsed command path, such as `worktree locate`.
pub fn find(path: &str) -> Option<&'static Verb> {
    DOMAINS
        .iter()
        .flat_map(|domain| domain.verbs)
        .find(|verb| verb.path.join(" ") == path)
}

/// A verb renders its own envelope, as the dispatch arm did, so its exit
/// status is the outcome it reported. `run` maps it back to the status
/// vocabulary; the mapping is exact because it is the published exit table.
pub(crate) fn rendered(status: u8) -> Result<Outcome, CommandError> {
    Ok(match status {
        0 => Outcome::Ok,
        1 => Outcome::Failed,
        2 => Outcome::Invalid,
        3 => Outcome::Error,
        4 => Outcome::Unavailable,
        5 => Outcome::Pending,
        124 => Outcome::Timeout,
        130 => Outcome::Cancelled,
        _ => Outcome::Error,
    })
}

/// A path reached a verb whose module does not own its command variant. The
/// mapping test in `workspace_policy` makes this unreachable in a release.
pub(crate) fn mismatched() -> Result<Outcome, CommandError> {
    Err(CommandError::new(
        "INTERNAL",
        "the command registry dispatched a command to the wrong domain",
        None,
        "report this internal dispatch error; rerun the command",
    ))
}
