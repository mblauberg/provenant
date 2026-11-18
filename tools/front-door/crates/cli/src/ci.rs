//! `provenant ci`: read CI evidence through one forge call.
use crate::metadata::CatalogueArg;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum Ci {
    /// Extract failing test names, panics and error lines from failed CI jobs
    #[command(add = crate::metadata::CoreCommand(11, "provenant ci failures pr 1947"))]
    Failures {
        /// Pull request numbers
        #[arg(
            long = "pr",
            value_name = "PR",
            num_args = 1..,
            action = clap::ArgAction::Append,
        )]
        pr: Vec<u64>,

        #[command(subcommand)]
        target: Option<FailureTarget>,
    },
}

#[derive(Debug, Subcommand)]
pub enum FailureTarget {
    /// Failing jobs of a pull request's checks
    Pr {
        /// Pull request numbers
        #[arg(catalog_u64_range = 1.., required = true)]
        numbers: Vec<u64>,
    },
    /// Failing jobs of one Actions run
    Run {
        /// Actions run id
        #[arg(catalog_u64_range = 1..)]
        run: u64,
    },
}
