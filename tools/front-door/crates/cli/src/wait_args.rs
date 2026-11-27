//! Command-line grammar for waiting on graphs and pull requests.
use crate::grammar::{CommonArgs, Target};
use crate::metadata::CatalogueArg;
use clap::{Args, ValueEnum};
use provenant_core::GitOid;

/// When a PR wait can settle after checks fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PrWaitUntil {
    /// Settle as soon as a check fails.
    Failure,
    /// Wait for all observed checks to finish.
    Complete,
}

/// `wait`: block until graph work or pull requests settle.
#[derive(Clone, Debug, Args)]
pub struct Wait {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Graphs, nodes or attempts (NAME[/NODE[#N]]); this project's running
    /// graphs when neither a target nor --pr is given
    #[arg(value_name = "TARGET", catalog_graph_target())]
    pub targets: Vec<Target>,
    /// Pull request whose required checks, merge or close ends the wait (repeatable)
    #[arg(long = "pr", value_name = "NUMBER", catalog_u64_range = 1..)]
    pub prs: Vec<u64>,
    /// Wait until GitHub reports this exact head; never verdict another SHA
    #[arg(long, value_name = "SHA", value_parser = parse_head_oid)]
    pub head: Option<String>,
    /// What this wait does when its pinned pull request head moves: pinned or follow_latest
    #[arg(long, value_name = "POLICY", value_parser = parse_pr_head_policy, requires = "prs", conflicts_with = "head")]
    pub pr_head_policy: Option<provenant_records::subject::SubjectPolicy>,
    /// PR check settlement: failure (default) or complete (waits for all checks)
    /// with a 60-minute budget from domains.wait.pr_complete_timeout_seconds;
    /// an explicit --timeout wins.
    #[arg(long, value_enum, requires = "prs")]
    pub until: Option<PrWaitUntil>,
    /// Wait until every target settles (the default)
    #[arg(long, conflicts_with_all = ["any", "fail_fast"])]
    pub all: bool,
    /// Return at the first target to settle or need input
    #[arg(long, conflicts_with_all = ["all", "fail_fast"])]
    pub any: bool,
    /// Return when a target fails, or when all targets settle
    #[arg(long = "fail-fast", conflicts_with_all = ["all", "any"])]
    pub fail_fast: bool,
    /// Print one line per transition until the wait ends
    #[arg(long)]
    pub follow: bool,
}

fn parse_head_oid(value: &str) -> Result<String, String> {
    GitOid::parse(value)
        .map(|_| value.to_owned())
        .map_err(|error| error.to_string())
}

/// Parse the declared wait policy, so a bad flag value is a grammar refusal
/// with the allowed spellings rather than a runtime default.
fn parse_pr_head_policy(value: &str) -> Result<provenant_records::subject::SubjectPolicy, String> {
    serde_json::from_value::<provenant_records::subject::SubjectPolicy>(serde_json::Value::String(
        value.to_owned(),
    ))
    .map_err(|_| "expected pinned or follow_latest".to_owned())
}
