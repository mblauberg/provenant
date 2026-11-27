//! `provenant ci`: read CI evidence through one forge call.
use crate::metadata::CatalogueArg;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Clone, Debug, Subcommand)]
pub enum Ci {
    /// Extract failing test names, panics and error lines from failed CI jobs
    #[command(add = crate::metadata::CoreCommand(11, "provenant ci failures pr 1947"))]
    Failures {
        #[command(flatten)]
        common: crate::grammar::CommonArgs,
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
    /// Retrieve a generated patch from CI for a pull request or run
    Patch(Patch),
}

#[derive(Clone, Debug, Args)]
pub struct Patch {
    #[command(flatten)]
    pub common: crate::grammar::CommonArgs,
    #[command(subcommand)]
    pub target: PatchTarget,
    /// Apply the verified patch to the selected worktree
    #[arg(long, global = true)]
    pub apply: bool,
    /// Worktree that must be at the manifest head
    #[arg(long, global = true, value_name = "PATH")]
    pub worktree: Option<PathBuf>,
    /// Commit only paths included in the patch
    #[arg(long, global = true, requires = "apply")]
    pub commit: bool,
}

#[derive(Clone, Debug, Subcommand)]
pub enum PatchTarget {
    /// Generated patch from the check run for this pull request
    Pr {
        #[arg(catalog_u64_range = 1..)]
        number: u64,
    },
    /// Generated patch from this Actions run
    Run {
        #[arg(catalog_u64_range = 1..)]
        id: u64,
    },
}

#[derive(Clone, Debug, Subcommand)]
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
