//! Worktree contracts and pure safety policy. Git mutation remains owner work.
use std::{ffi::OsString,path::PathBuf};
use thiserror::Error;
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("worktree name must be 1-64 safe filename characters without slashes")]
    InvalidName,
    #[error("git worktree list output was incomplete")]
    Incomplete,
    #[error("git worktree list output was malformed: {0}")]
    Malformed(String),
    #[error("detached worktrees require a name")]
    DetachedName,
}

/// Values are either the source's True or lossless surrogateescaped text. This
/// is an internal OS contract; a JSON path codec is still needed at the edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PorcelainValue {
    Flag,
    Text(OsString),
}
/// Insertion order is retained, including optional fields and flag-vs-text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub fields: Vec<(String, PorcelainValue)>,
}
#[derive(Debug, clap::Parser)]
#[command(name = "worktree", args_override_self = true)]
pub struct Arguments {
    #[command(subcommand)]
    pub command: Operation,
}
#[derive(Debug, clap::Subcommand)]
pub enum Operation {
    Create(Create),
    List(Repository),
    Check(Repository),
    ValidateContext {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        allow_non_git: bool,
    },
    VerifyClaim {
        #[arg(long)]
        expected_worktree: PathBuf,
        #[arg(long)]
        claimed_worktree: PathBuf,
        #[arg(long)]
        claimed_commit: Option<String>,
        #[arg(long)]
        base_revision: Option<String>,
        #[arg(long)]
        expected_common: Option<PathBuf>,
    },
    Remove {
        name: String,
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
}
#[derive(Debug, clap::Args)]
pub struct Repository {
    #[arg(long)]
    pub repo: Option<PathBuf>,
}
#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("mode").required(true).args(["detach","existing_branch","new_branch"])))]
pub struct Create {
    pub name: Option<String>,
    #[arg(long)]
    pub repo: Option<PathBuf>,
    #[arg(long)]
    pub no_node_modules: bool,
    #[arg(long)]
    pub detach: Option<String>,
    #[arg(long)]
    pub existing_branch: Option<String>,
    #[arg(long)]
    pub new_branch: Option<String>,
    #[arg(long, default_value = "HEAD")]
    pub start_point: String,
}
/// Exact verify_claim result variants. Accepted records have no reason and
/// rejected records have none of the acceptance fields.
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ClaimReceipt {
    Rejected {
        reason: String,
    },
    Accepted {
        base_revision: String,
        head_revision: String,
        clean: bool,
        claimed_commit: String,
        claimed_worktree: String,
        common_git_dir: String,
        acceptance_owner: String,
        acceptance_mode: String,
    },
}

/// Exact records returned by create/remove/check; JSON path rendering is pending.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Created {
    pub status: String,
    pub name: String,
    pub primary_root: String,
    pub worktree_root: String,
    pub common_git_dir: String,
    pub head_revision: String,
    pub branch: Option<String>,
    pub detached: bool,
    pub node_modules: String,
    pub node_modules_reason: Option<String>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Removed {
    pub status: String,
    pub name: String,
    pub primary_root: String,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Checked {
    pub status: String,
    pub primary_root: String,
    pub findings: Vec<String>,
}
