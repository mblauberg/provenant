//! `provenant ci`: read CI evidence through one forge call.
use crate::metadata::CatalogueArg;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum Ci {
    /// Extract failing test names, panics and error lines from failed CI jobs
    #[command(add = crate::metadata::CoreCommand(11, "provenant ci failures pr 1947"))]
    Failures {
        #[command(subcommand)]
        target: FailureTarget,
    },
}

#[derive(Debug, Subcommand)]
pub enum FailureTarget {
    /// Failing jobs of a pull request's checks
    Pr {
        /// Pull request number
        #[arg(catalog_u64_range = 1..)]
        number: u64,
    },
    /// Failing jobs of one Actions run
    Run {
        /// Actions run id
        #[arg(catalog_u64_range = 1..)]
        run: u64,
    },
}
