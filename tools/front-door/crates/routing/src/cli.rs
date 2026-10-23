//! Typed argument surface for the `model-route` compatibility command.

use std::ffi::OsString;

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum RiskTier {
    Routine,
    Substantial,
    Crucial,
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum FallbackChoice {
    True,
    False,
    Any,
}

#[derive(Clone, Debug, Eq, PartialEq, Parser)]
#[command(
    name = "model-route",
    about = "Resolve durable harness model aliases into auditable concrete routes"
)]
pub struct RouteCli {
    #[command(subcommand)]
    pub command: RouteCommand,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum RouteCommand {
    Resolve(Box<ResolveArgs>),
    Snapshot(JsonArgs),
    Health(JsonArgs),
    Probe(ProbeArgs),
    Select(SelectArgs),
    Routes(RoutesArgs),
    Cooldowns(JsonArgs),
    Reset(ResetArgs),
    Pick(PickArgs),
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct ResolveArgs {
    #[arg(long)]
    pub adapter: Option<String>,
    #[arg(long)]
    pub alias: Option<String>,
    #[arg(long)]
    pub task_class: Option<String>,
    #[arg(long, help = "comma-separated classifier stages for this call")]
    pub classify: Option<String>,
    #[arg(long, hide = true, default_value = "")]
    pub brief: String,
    #[arg(long, hide = true, default_value = "")]
    pub mode: String,
    #[arg(long, hide = true)]
    pub path: Vec<String>,
    #[arg(long, hide = true)]
    pub size_files: Option<i64>,
    #[arg(long, hide = true)]
    pub size_lines: Option<i64>,
    #[arg(long, help = "record why this route escalates to principal")]
    pub principal_reason: Option<String>,
    #[arg(long, value_enum)]
    pub model_override_tier: Option<RiskTier>,
    #[arg(long, default_value = "")]
    pub role: String,
    #[arg(long)]
    pub effort: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long, value_enum)]
    pub fallback: Option<FallbackChoice>,
    #[arg(long = "fallback-route")]
    pub fallback_route: Vec<String>,
    #[arg(long = "available-model")]
    pub available_model: Vec<String>,
    #[arg(long = "available-effort")]
    pub available_effort: Vec<String>,
    #[arg(long)]
    pub capabilities_file: Option<String>,
    #[arg(long)]
    pub lead_family: Option<String>,
    #[arg(long)]
    pub endpoint: Option<String>,
    #[arg(long)]
    pub require_distinct: bool,
    #[arg(long, hide = true)]
    pub catalog: Option<String>,
    #[arg(long, hide = true)]
    pub adapter_compatibility: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct JsonArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct ProbeArgs {
    #[arg(long, required = true)]
    pub adapter: String,
    #[arg(long, required = true)]
    pub executable: String,
    #[arg(long)]
    pub json: bool,
    #[arg(
        long,
        help = "list models only, within a 30-second deadline (discovery)"
    )]
    pub listing_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct SelectArgs {
    #[arg(long, hide = true)]
    pub catalog: Option<String>,
    #[arg(long, required = true)]
    pub task_class: String,
    #[arg(long, required = true)]
    pub role: String,
    #[arg(long, required = true)]
    pub candidates_file: String,
    #[arg(long)]
    pub preferences_file: Option<String>,
    #[arg(long, required = true)]
    pub spread_state_file: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct RoutesArgs {
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub health: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct ResetArgs {
    pub selector: Option<String>,
    #[arg(long)]
    pub account: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct PickArgs {
    #[arg(long, hide = true)]
    pub seed: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedCommand {
    Command(Box<RouteCommand>),
    LegacyPick(String),
}

const COMMANDS: [&str; 9] = [
    "resolve",
    "snapshot",
    "health",
    "probe",
    "select",
    "routes",
    "cooldowns",
    "reset",
    "pick",
];

pub fn parse_command<I, S>(args: I) -> Result<ParsedCommand, clap::Error>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    if let Some(first) = args.first().and_then(|arg| arg.to_str()) {
        if !first.starts_with('-') && !COMMANDS.contains(&first) {
            return Ok(ParsedCommand::LegacyPick(first.to_owned()));
        }
    }
    RouteCli::try_parse_from(std::iter::once(OsString::from("model-route")).chain(args))
        .map(|cli| ParsedCommand::Command(Box::new(cli.command)))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HealthRecord {
    pub schema: String,
    pub task_class_routes: Value,
    pub routes: Value,
}

pub fn health_record(catalog: &Value, route_health: Option<Value>) -> Value {
    serde_json::to_value(HealthRecord {
        schema: "fabric.route-health.v1".into(),
        task_class_routes: catalog
            .get("task_class_routes")
            .cloned()
            .unwrap_or_else(|| json!({})),
        routes: route_health.unwrap_or_else(|| json!({})),
    })
    .unwrap_or_else(
        |_| json!({"schema":"fabric.route-health.v1","task_class_routes":{},"routes":{}}),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ParsedCommand, RouteCommand, health_record, parse_command};

    #[test]
    fn subcommands_parse_and_bare_route_names_remain_supported() {
        assert!(matches!(
            parse_command(["routes", "--all"]).unwrap(),
            ParsedCommand::Command(command) if matches!(*command, RouteCommand::Routes(_))
        ));
        assert_eq!(
            parse_command(["senior"]).unwrap(),
            ParsedCommand::LegacyPick("senior".into())
        );
    }

    #[test]
    fn health_record_defaults_to_empty_health_when_the_store_fails() {
        let record = health_record(
            &json!({"task_class_routes":{"build":{"alias":"senior"}}}),
            None,
        );
        assert_eq!(record["schema"], "fabric.route-health.v1");
        assert_eq!(record["routes"], json!({}));
        assert_eq!(record["task_class_routes"]["build"]["alias"], "senior");
    }
}
