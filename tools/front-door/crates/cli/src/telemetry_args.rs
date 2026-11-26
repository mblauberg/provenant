//! Arguments of `telemetry value` and `telemetry cost`.
use crate::{Target, metadata::CatalogueArg};
use clap::{Args, ValueEnum};

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
