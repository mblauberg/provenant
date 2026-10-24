//! Frozen benchmark command grammar and split selection.
use clap::{Args, Subcommand, ValueEnum};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub repo: String,
    pub parent_sha: String,
    pub merge_sha: String,
    pub prompt: String,
    pub hidden_test_command: String,
    pub split: Split,
}

impl<'de> Deserialize<'de> for Split {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "search" => Ok(Self::Search),
            "acceptance" => Ok(Self::Acceptance),
            "held-out" => Ok(Self::HeldOut),
            _ => Err(serde::de::Error::custom("invalid benchmark split")),
        }
    }
}

pub fn parse_task(text: &str) -> Result<Task, toml_edit::de::Error> {
    toml_edit::de::from_str(text)
}

#[derive(Debug, Subcommand)]
pub enum Bench {
    /// Propose tasks from merge commits that changed Rust tests.
    Mine(Mine),
    /// Check that a frozen task's tests fail on its parent and pass on its merge.
    Verify { task: PathBuf },
    /// Replay tasks through the selected dispatch route.
    Run(Run),
}

#[derive(Debug, Args)]
pub struct Mine {
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=100))]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum Split {
    Search,
    Acceptance,
    HeldOut,
}

#[derive(Debug, Args)]
pub struct Run {
    /// Route name or model, optionally followed by @effort.
    #[arg(long)]
    pub route: String,
    #[arg(long, value_enum, default_value = "acceptance")]
    pub split: Split,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=100))]
    pub limit: Option<u64>,
}

/// Default runs deliberately exclude held-out tasks.
pub fn selected_split(requested: Option<Split>) -> Split {
    requested.unwrap_or(Split::Acceptance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, ParseOutcome};
    use std::ffi::OsString;

    #[test]
    fn task_split_defaults_to_acceptance_and_accepts_held_out() {
        assert_eq!(selected_split(None), Split::Acceptance);
        assert_eq!(selected_split(Some(Split::HeldOut)), Split::HeldOut);
        let parsed = crate::parse([
            OsString::from("bench"),
            OsString::from("run"),
            OsString::from("--route"),
            OsString::from("codex/gpt-6-luna@high"),
            OsString::from("--split"),
            OsString::from("held-out"),
        ]);
        assert!(matches!(parsed, ParseOutcome::Invocation(ref invocation)
            if matches!(invocation.command, Command::Bench(Bench::Run(Run { split: Split::HeldOut, .. })))));
        let task = parse_task(
            r#"repo = "."
parent_sha = "abc"
merge_sha = "def"
prompt = "fix it"
hidden_test_command = "cargo test"
split = "held-out"
"#,
        );
        assert!(matches!(
            task,
            Ok(Task {
                split: Split::HeldOut,
                ..
            })
        ));
    }
}
