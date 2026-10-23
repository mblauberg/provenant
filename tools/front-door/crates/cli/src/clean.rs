//! Cleanup command contract. Planning, merge proof and deletion stay with owners.
use crate::CommandOutput;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};
use thiserror::Error;

pub const DEFAULT_INCLUDE: &str = "runs,scratch,worktrees";

#[derive(Debug, Parser)]
#[command(name = "clean")]
pub struct Arguments {
    #[arg(long)]
    pub repo: Option<PathBuf>,
    #[arg(long, default_value = DEFAULT_INCLUDE)]
    pub include: String,
    #[arg(long, value_parser = duration)]
    pub older_than: Option<f64>,
    #[arg(long)]
    pub apply: bool,
    #[arg(long, value_name = "sha256:DIGEST")]
    pub plan: Option<String>,
    #[arg(long)]
    pub prune_merged: bool,
    #[arg(long, value_name = "NAME", conflicts_with = "merged_since")]
    pub branch: Vec<String>,
    #[arg(long, value_name = "REV")]
    pub merged_since: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RequestError {
    #[error("--apply and --plan sha256:<digest> are required together")]
    ApprovalPair,
    #[error("--prune-merged accepts only --repo, --json, and a merge selector")]
    PruneOptions,
    #[error("--prune-merged requires --branch NAME or --merged-since REV")]
    MissingSelector,
    #[error("merge selectors require --prune-merged")]
    UnexpectedSelector,
}

#[derive(Debug, PartialEq)]
pub enum Operation {
    Plan,
    Apply {
        digest: String,
    },
    Prune {
        branches: Vec<String>,
        merged_since: Option<String>,
    },
}

#[derive(Debug, PartialEq)]
pub struct Request {
    pub repo: PathBuf,
    pub include: BTreeSet<String>,
    pub older_than_days: Option<f64>,
    pub json: bool,
    pub operation: Operation,
}

impl Arguments {
    /// Cwd is explicit; parsing never observes or mutates process globals.
    pub fn request(self, cwd: PathBuf) -> Result<Request, RequestError> {
        let has_plan = self.plan.as_ref().is_some_and(|s| !s.is_empty());
        if self.apply != has_plan {
            return Err(RequestError::ApprovalPair);
        }
        if self.prune_merged
            && (self.apply || self.older_than.is_some() || self.include != DEFAULT_INCLUDE)
        {
            return Err(RequestError::PruneOptions);
        }
        let selector =
            !self.branch.is_empty() || self.merged_since.as_ref().is_some_and(|s| !s.is_empty());
        if self.prune_merged && !selector {
            return Err(RequestError::MissingSelector);
        }
        if !self.prune_merged && selector {
            return Err(RequestError::UnexpectedSelector);
        }
        let operation = if self.prune_merged {
            Operation::Prune {
                branches: self.branch,
                merged_since: self.merged_since,
            }
        } else if self.apply {
            Operation::Apply {
                digest: self.plan.unwrap_or_default(),
            }
        } else {
            Operation::Plan
        };
        Ok(Request {
            repo: self.repo.unwrap_or(cwd),
            include: self
                .include
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            older_than_days: self.older_than,
            json: self.json,
            operation,
        })
    }
}

pub fn duration(value: &str) -> Result<f64, String> {
    let error = || "duration must be like 7d or 24h".to_owned();
    let unit = value.chars().last().ok_or_else(error)?;
    if !matches!(unit, 'd' | 'h') {
        return Err(error());
    }
    let digits = &value[..value.len() - 1];
    if digits.is_empty() {
        return Err(error());
    }
    let mut number = 0.0;
    for c in digits.chars() {
        let digit = crate::pending::decimal_digit(c).ok_or_else(error)?;
        number = number * 10.0 + f64::from(digit);
    }
    // Python converts to float when multiplying by 1/24, but keeps day values
    // as integers; extremely large durations are not yet part of this interface.
    if !number.is_finite() {
        return Err(error());
    }
    Ok(number * if unit == 'd' { 1.0 } else { 1.0 / 24.0 })
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RemovalReport {
    pub removed: Vec<String>,
    pub skipped: Vec<Skipped>,
}

/// Removal output has two distinct text contracts. JSON records remain owner data.
pub fn render_removals(report: &RemovalReport, prune: bool) -> CommandOutput {
    let mut out = CommandOutput::default();
    if !prune {
        out.stdout
            .push_str(&format!("removed {} paths\n", report.removed.len()));
    }
    for path in &report.removed {
        if prune {
            out.stdout.push_str("removed ");
        }
        out.stdout.push_str(path);
        out.stdout.push('\n');
    }
    for row in &report.skipped {
        out.stdout
            .push_str(&format!("skipped {}: {}\n", row.path, row.reason));
    }
    out
}

pub fn render_error(error: &impl std::fmt::Display) -> CommandOutput {
    CommandOutput {
        stderr: format!("provenant clean: {error}\n"),
        exit_code: 2,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(words: &[&str]) -> Arguments {
        Arguments::try_parse_from(std::iter::once("clean").chain(words.iter().copied())).unwrap()
    }

    #[test]
    fn approval_and_prune_selectors_are_validated_in_source_order() {
        for words in [vec!["--apply"], vec!["--plan", "sha256:x"]] {
            assert_eq!(
                args(&words).request("/r".into()).unwrap_err(),
                RequestError::ApprovalPair
            );
        }
        assert_eq!(
            args(&["--prune-merged"]).request("/r".into()).unwrap_err(),
            RequestError::MissingSelector
        );
        assert_eq!(
            args(&["--branch", "x"]).request("/r".into()).unwrap_err(),
            RequestError::UnexpectedSelector
        );
        assert_eq!(
            args(&["--prune-merged", "--older-than", "7d"])
                .request("/r".into())
                .unwrap_err(),
            RequestError::PruneOptions
        );
        assert!(
            Arguments::try_parse_from(["clean", "--branch", "x", "--merged-since", "y"]).is_err()
        );
    }

    #[test]
    fn defaults_repeated_branches_and_include_normalization() {
        let request = args(&["--include", " runs, scratch,,runs ", "--older-than", "24h"])
            .request("/cwd".into())
            .unwrap();
        assert_eq!(request.repo, PathBuf::from("/cwd"));
        assert_eq!(
            request.include,
            BTreeSet::from(["runs".into(), "scratch".into()])
        );
        assert_eq!(request.older_than_days, Some(1.0));
        assert_eq!(
            args(&["--prune-merged", "--branch", "a", "--branch", "b"])
                .request("/r".into())
                .unwrap()
                .operation,
            Operation::Prune {
                branches: vec!["a".into(), "b".into()],
                merged_since: None
            }
        );
    }

    #[test]
    fn duration_contract() {
        for value in ["7", "-1d", "1.5d", "d", "7w"] {
            assert!(duration(value).is_err());
        }
        assert_eq!(duration("7d").unwrap(), 7.0);
        assert_eq!(duration("٠٢٤h").unwrap(), 1.0);
    }

    #[test]
    fn removal_modes_and_failures_preserve_text_and_exit() {
        let report = RemovalReport {
            removed: vec!["a".into()],
            skipped: vec![Skipped {
                path: "b".into(),
                reason: "keep:active".into(),
            }],
        };
        assert_eq!(
            render_removals(&report, false).stdout,
            "removed 1 paths\na\nskipped b: keep:active\n"
        );
        assert_eq!(
            render_removals(&report, true).stdout,
            "removed a\nskipped b: keep:active\n"
        );
        assert_eq!(render_error(&RequestError::MissingSelector).exit_code, 2);
    }
}
