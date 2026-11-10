//! Arguments of `telemetry value` and `telemetry cost`.
use crate::Target;
use clap::{Args, ValueEnum};

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
pub struct TelemetryCost {
    /// Count only this attempt, task or run; default every attempt in the ledger
    pub target: Option<Target>,
    /// Break the cost down; attempts with no value form an `unattributed` group
    #[arg(long, value_enum)]
    pub by: Option<CostBy>,
    /// Count attempts finished from this date or timestamp, or a span such as 7d
    #[arg(long, value_parser = crate::grammar::since)]
    pub since: Option<jiff::Timestamp>,
}
