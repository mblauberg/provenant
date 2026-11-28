//! Telemetry command tree and arguments.
use crate::{
    Target,
    grammar::{since, timestamp},
    metadata::CatalogueArg,
};
use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Clone, Debug, Args)]
pub struct TelemetryValue {
    /// Account label or key
    #[arg(long)]
    pub account: Option<String>,
    /// Quota window, such as five_hour or weekly (default every window)
    #[arg(long)]
    pub window: Option<String>,
    /// Count usage from this date or timestamp, or a span such as 7d (default 30d)
    #[arg(long, value_parser = crate::grammar::since)]
    pub since: Option<jiff::Timestamp>,
}

/// What `telemetry cost --by` groups on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum CostBy {
    Route,
    Account,
    Model,
    Graph,
    Node,
}

impl CostBy {
    /// The accepted groupings, in runbook order. One declaration for clap,
    /// the reducer and the doc-drift test.
    pub const ALL: &'static [Self] = &[
        Self::Route,
        Self::Account,
        Self::Model,
        Self::Graph,
        Self::Node,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_groupings_match_the_runbook() -> Result<(), Box<dyn std::error::Error>> {
        const DOC: &str = include_str!("../../../../../docs/runbooks/telemetry.md");
        assert_eq!(CostBy::value_variants().len(), CostBy::ALL.len());
        let accepted: Vec<String> = CostBy::ALL
            .iter()
            .filter_map(|by| by.to_possible_value())
            .map(|value| value.get_name().to_owned())
            .collect();
        let line = DOC
            .lines()
            .find(|line| line.contains("telemetry cost "))
            .ok_or("runbook has no telemetry cost line")?;
        let after = line
            .split_once("--by")
            .ok_or("runbook cost line has no --by")?
            .1;
        let documented: Vec<String> = after
            .trim_start()
            .split([']', '`'])
            .next()
            .ok_or("runbook --by has no list")?
            .split('|')
            .map(|name| name.trim().to_owned())
            .collect();
        assert_eq!(accepted, documented);
        assert_eq!(accepted.len(), CostBy::ALL.len());
        Ok(())
    }
}

#[derive(Clone, Debug, Args)]
pub struct TelemetryCost {
    /// Count only this graph, node or attempt (NAME[/NODE[#N]]); default every attempt in the ledger
    #[arg(value_name = "TARGET", catalog_graph_target())]
    pub target: Option<Target>,
    /// Break the cost down; attempts with no value form an `unattributed` group
    #[arg(long, value_enum)]
    pub by: Option<CostBy>,
    /// Count attempts finished from this date or timestamp, or a span such as 7d
    #[arg(long, value_parser = crate::grammar::since)]
    pub since: Option<jiff::Timestamp>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Telemetry {
    /// Summarise local attempts and usage
    Stats(Stats),
    /// Write attempts as a Chrome Trace Event file for ui.perfetto.dev
    Trace(Trace),
    /// Maintain the telemetry price sheet
    Prices {
        #[command(subcommand)]
        command: Prices,
    },
    /// Record native chair and sub-agent token usage per account
    Ingest(Ingest),
    /// Report quota mileage from quota history and ingested usage
    Mileage(Mileage),
    /// Compare each account's API-equivalent lane usage with its plan price and quota readings
    Value(crate::telemetry_args::TelemetryValue),
    /// Report API-equivalent cost from the usage ledger, for one attempt or grouped
    Cost(crate::telemetry_args::TelemetryCost),
    /// Compare experiment arms using offline pipeline outcomes
    #[command(subcommand, visible_alias = "exp")]
    Experiments(Experiments),
}
#[derive(Clone, Debug, Subcommand)]
pub enum Experiments {
    /// Show one experiment's stratified arm comparison
    Show(ExperimentShow),
}
#[derive(Clone, Debug, Args)]
pub struct ExperimentShow {
    /// Historical experiment name recorded in offline outcomes
    pub id: String,
    #[command(flatten)]
    pub pipeline: PipelineOptions,
}
#[derive(Clone, Debug, Subcommand)]
pub enum Prices {
    /// Fetch market prices and write a proposed diff; never edits the sheet
    Refresh(PricesRefresh),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PriceSource {
    Openrouter,
    Litellm,
}
#[derive(Clone, Debug, Args)]
pub struct PricesRefresh {
    /// Primary source; the other is the cross-check (default openrouter)
    #[arg(long, value_enum, default_value = "openrouter")]
    pub source: PriceSource,
    /// Read the OpenRouter models document from this file instead of fetching it
    #[arg(long)]
    pub openrouter_file: Option<PathBuf>,
    /// Read the LiteLLM price document from this file instead of fetching it
    #[arg(long)]
    pub litellm_file: Option<PathBuf>,
    /// Proposal file (default `telemetry-prices.proposed.json`)
    #[arg(long, default_value = "telemetry-prices.proposed.json")]
    pub out: PathBuf,
}
#[derive(Clone, Debug, Args)]
pub struct Ingest {
    /// Ingest only this provider's sessions (only `claude` writes native transcripts)
    #[arg(long)]
    pub provider: Option<String>,
    /// Ingest transcripts written, and messages stamped, from this date or timestamp, or a span such as 1d ago
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
    /// Resume from the durable cursor instead of rescanning a time window
    #[arg(long)]
    pub incremental: bool,
}
#[derive(Clone, Debug, Args)]
pub struct Mileage {
    /// Account label or key
    #[arg(long)]
    pub account: Option<String>,
    /// Quota window, such as five_hour or weekly (default every window)
    #[arg(long)]
    pub window: Option<String>,
    /// Count only usage of this exact model; quota deltas still cover every model, so mileage is understated
    #[arg(long)]
    pub model: Option<String>,
    /// Use readings and usage from this date or timestamp, or a span such as 1d ago
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
    /// Runs needed on each side before a capacity trend is judged
    #[arg(long, default_value_t = 10, catalog_u32_range = 2..=10_000)]
    pub min_runs: u32,
    /// Fall in capacity, in percent, that is flagged rather than inconclusive
    #[arg(long, default_value_t = 20, catalog_u32_range = 1..=99)]
    pub fall_percent: u32,
    /// Days in the recent side of a capacity step change
    #[arg(long, default_value_t = 7, catalog_u32_range = 1..=90)]
    pub recent_days: u32,
}
#[derive(Clone, Debug, Args)]
pub struct Trace {
    /// Output file (default `provenant-trace.json`)
    #[arg(long, default_value = "provenant-trace.json")]
    pub out: std::path::PathBuf,
    /// Include attempts finished from this date or timestamp, or a span such as 1d ago
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
    /// Restrict the trace to one graph, node or attempt (NAME[/NODE[#N]])
    #[arg(value_name = "TARGET", catalog_graph_target())]
    pub target: Option<Target>,
    /// Also read the attempts of runs placed on peers, one SSH status read per peer
    #[arg(long)]
    pub peers: bool,
}
#[derive(Clone, Debug, Args)]
pub struct Stats {
    /// Include offline post-merge outcomes and stratified ranking
    #[arg(long, conflicts_with = "usage")]
    pub outcomes: bool,
    /// Include advisory expiring suggestions from offline pipeline evidence
    #[arg(long, conflicts_with = "usage")]
    pub suggest: bool,
    #[command(flatten)]
    pub pipeline: PipelineOptions,
    /// Count local CLI verbs, including unused installed units.
    #[arg(long)]
    pub usage: bool,
    /// Count from this date or timestamp, or a span such as 1d ago
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
    /// Count up to this date or timestamp
    #[arg(long, value_parser = timestamp)]
    pub until: Option<jiff::Timestamp>,
}

#[derive(Clone, Debug, Args)]
pub struct PipelineOptions {
    /// Local main-line ref to analyse; no fetch is performed
    #[arg(long, default_value = "main")]
    pub main_ref: String,
    /// Recorded gh main-branch run list JSON; absent means CI not_measured
    #[arg(long)]
    pub ci_file: Option<PathBuf>,
    /// Days after a merge to attribute escaped defects
    #[arg(long, default_value_t = 14, catalog_u32_range = 1..=3650)]
    pub outcome_days: u32,
    /// Independent graphs and mature merges required per arm
    #[arg(long, default_value_t = 20, catalog_u32_range = 1..=100_000)]
    pub min_samples: u32,
    /// Days until a suggestion expires
    #[arg(long, default_value_t = 14, catalog_u32_range = 1..=3650)]
    pub expiry_days: u32,
}
