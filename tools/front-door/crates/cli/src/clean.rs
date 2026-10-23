//! Cleanup command contract. Planning, merge proof and deletion stay with owners.
use crate::CommandOutput;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};
use thiserror::Error;

pub const DEFAULT_INCLUDE: &str = "runs,scratch,worktrees";

#[derive(Debug, Parser)]
#[command(name = "clean", args_override_self = true)]
pub struct Arguments {
    #[arg(long)]
    pub repo: Option<PathBuf>,
    #[arg(long, default_value = DEFAULT_INCLUDE)]
    pub include: String,
    #[arg(long, value_parser = duration)]
    pub older_than: Option<DurationDays>,
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
    pub older_than_days: Option<DurationDays>,
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
                .map(provenant_core::py::python_strip)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            older_than_days: self.older_than,
            json: self.json,
            operation,
        })
    }
}

/// Day durations remain exact integers; hour durations retain Python's float result.
#[derive(Debug, Clone, PartialEq)]
pub enum DurationDays {
    Integer(u64),
    Fractional(f64),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DurationError {
    #[error("duration must be like 7d or 24h")]
    Invalid,
    #[error(transparent)]
    Integer(#[from] crate::IntegerError),
}

pub fn duration(value: &str) -> Result<DurationDays, DurationError> {
    let unit = value.chars().last().ok_or(DurationError::Invalid)?;
    if !matches!(unit, 'd' | 'h') {
        return Err(DurationError::Invalid);
    }
    let digits = &value[..value.len() - 1];
    if digits.is_empty()
        || !digits
            .chars()
            .all(|c| crate::pending::decimal_digit(c).is_some())
    {
        return Err(DurationError::Invalid);
    }
    let number = crate::pending::decimal_integer(digits)?;
    if unit == 'd' {
        Ok(DurationDays::Integer(number))
    } else {
        Ok(DurationDays::Fractional(number as f64 * (1.0 / 24.0)))
    }
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

/// Cleanup's actual retained/public shapes. Policy and deletion remain owner work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Row {
    pub path: String,
    pub kind: String,
    pub age_days: f64,
    pub size_bytes: u64,
    pub verdict: String,
    #[serde(rename = "_identity")]
    pub identity: (u64, u64, i64, u64),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRow {
    pub path: String,
    pub kind: String,
    pub verdict: String,
    #[serde(rename = "_identity")]
    pub identity: (u64, u64, i64, u64),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Integration {
    pub integration_ref: String,
    pub integration_revision: String,
    pub branches: Vec<String>,
    // Present null when --branch, rather than --merged-since, selected the worktrees.
    pub merged_since_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum RetentionDays {
    Integer(u64),
    Fractional(f64),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Approval {
    pub root: String,
    pub include: Vec<String>,
    pub older_than: Option<RetentionDays>,
    pub rows: Vec<ApprovalRow>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub integration: Option<Integration>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SavedPlan {
    pub plan_sha256: String,
    pub approval: Approval,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlanContext {
    pub prune_merged: bool,
    pub since_revision: Option<String>,
    pub refs: Option<String>,
    pub pr_unknown: bool,
    pub open_heads: BTreeSet<String>,
    pub integration_ref: Option<String>,
    pub older_than: Option<RetentionDays>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SizedPath {
    pub path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SizeWarning {
    pub message: String,
    pub largest: Vec<SizedPath>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlanReport {
    pub root: String,
    pub rows: Vec<Row>,
    pub reclaimable_bytes: u64,
    pub plan_sha256: String,
    pub approval: Approval,
    #[serde(rename = "_context")]
    pub context: PlanContext,
    pub warnings: Vec<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub integration: Option<Integration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<SizeWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublicRow {
    pub path: String,
    pub kind: String,
    pub age_days: f64,
    pub size_bytes: u64,
    pub verdict: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PublicPlan {
    pub root: String,
    pub rows: Vec<PublicRow>,
    pub reclaimable_bytes: u64,
    pub plan_sha256: String,
    pub warnings: Vec<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub integration: Option<Integration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<SizeWarning>,
}

impl PlanReport {
    pub fn public(&self) -> PublicPlan {
        PublicPlan {
            root: self.root.clone(),
            rows: self
                .rows
                .iter()
                .map(|row| PublicRow {
                    path: row.path.clone(),
                    kind: row.kind.clone(),
                    age_days: row.age_days,
                    size_bytes: row.size_bytes,
                    verdict: row.verdict.clone(),
                })
                .collect(),
            reclaimable_bytes: self.reclaimable_bytes,
            plan_sha256: self.plan_sha256.clone(),
            warnings: self.warnings.clone(),
            integration: self.integration.clone(),
            warning: self.warning.clone(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyReport {
    pub removed: Vec<String>,
    pub count: u64,
    pub skipped: Vec<Skipped>,
}

// Untagged wire shapes, exactly as clean.py prints them; no enum wrapper.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PublicReport {
    Plan(PublicPlan),
    Applied(ApplyReport),
    Pruned(RemovalReport),
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

pub fn render_operational_error(error: &impl std::fmt::Display) -> CommandOutput {
    CommandOutput {
        stderr: format!("provenant clean: {error}\n"),
        exit_code: 2,
        ..Default::default()
    }
}

/// Parser usage is supplied by the edge presentation owner. The missing-selector
/// refusal is operational text in Python; the other request failures are parser errors.
pub fn render_request_error(error: &RequestError, program: &str, usage: &str) -> CommandOutput {
    if *error == RequestError::MissingSelector {
        return render_operational_error(error);
    }
    CommandOutput {
        stderr: format!("{usage}{program}: error: {error}\n"),
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
    fn retained_and_public_plan_contracts_match_python_records() {
        use serde_json::{json, Value};
        let dir = tempfile::tempdir().unwrap();
        let Some(expected): Option<(Value, Value)> = crate::oracle::python(
            r#"
import json,sys,os,subprocess
from pathlib import Path
from datetime import datetime,timezone
from scripts.clean import plan
root=Path(json.load(sys.stdin)['root']).resolve()
# TMPDIR can sit inside a live checkout. Establish an isolated repository so
# primary_root cannot discover and scan that enclosing project's run tree.
subprocess.run(['git','init','-q',str(root)],check=True,capture_output=True,timeout=10)
p=root/'.agent-run/scratch/old.txt'; p.parent.mkdir(parents=True); p.write_text('fixture');os.utime(p,(100000,100000))
# Only this isolated repository supplies cleanup observations.
r=plan(root,pr_bodies=[],now=datetime.fromtimestamp(1000000,timezone.utc))
public={**{k:v for k,v in r.items() if k not in {'approval','_context'}},'rows':[{k:v for k,v in row.items() if k!='_identity'} for row in r['rows']]}
r['_context']['open_heads']=sorted(r['_context']['open_heads'])
print(json.dumps([r,public]))
"#,
            &json!({"root":dir.path()}),
        ) else {
            return;
        };
        let report: PlanReport = serde_json::from_value(expected.0.clone()).unwrap();
        assert_eq!(serde_json::to_value(&report).unwrap(), expected.0);
        assert_eq!(serde_json::to_value(report.public()).unwrap(), expected.1);
        let report: PublicReport = serde_json::from_value(expected.1.clone()).unwrap();
        assert_eq!(serde_json::to_value(report).unwrap(), expected.1);
        let base = json!({"root":"/r","include":["worktrees"],"older_than":null,"rows":[],"integration_ref":"origin/main","integration_revision":"abc","branches":["topic"],"merged_since_revision":null});
        let approval: Approval = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(serde_json::to_value(approval).unwrap(), base);
        for record in [
            json!({"removed":["p"],"count":1,"skipped":[]}),
            json!({"removed":[],"skipped":[{"path":"p","reason":"keep:active"}]}),
        ] {
            let report: PublicReport = serde_json::from_value(record.clone()).unwrap();
            assert_eq!(serde_json::to_value(report).unwrap(), record);
        }
    }

    #[test]
    fn duration_refuses_python_digit_limit() {
        assert!(duration(&format!("{}d", "7".repeat(4301))).is_err());
        assert!(duration(&format!("{}h", "0".repeat(4301))).is_err());
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
        assert_eq!(request.older_than_days, Some(DurationDays::Fractional(1.0)));
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
        assert_eq!(duration("7d").unwrap(), DurationDays::Integer(7));
        assert_eq!(
            duration("9007199254740993d").unwrap(),
            DurationDays::Integer(9007199254740993)
        );
        assert_eq!(duration("٠٢٤h").unwrap(), DurationDays::Fractional(1.0));
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
        assert_eq!(
            render_request_error(&RequestError::MissingSelector, "clean.py", "usage\n").exit_code,
            2
        );
    }
    #[test]
    fn invalid_request_error_classes_match_python_streams_and_exit_codes() {
        use serde_json::json;
        let cases = [
            vec!["--apply"],
            vec!["--prune-merged", "--include", "runs", "--branch", "x"],
            vec!["--branch", "x"],
            vec!["--prune-merged"],
        ];
        let Some(expected): Option<Vec<(String,String,u8)>> = crate::oracle::python(
            "import json,subprocess,sys; print(json.dumps([(r.stdout,r.stderr,r.returncode) for a in json.load(sys.stdin) for r in [subprocess.run([sys.executable,'scripts/clean.py',*a],capture_output=True,text=True)]]))", &json!(cases)
        ) else { return; };
        for (argv, (stdout, stderr, exit_code)) in cases.iter().zip(expected) {
            let error = args(argv).request("/r".into()).unwrap_err();
            // Usage is an explicit parser-owned input. This tests renderer classification,
            // stdout/stderr framing and exit status, not deferred help layout generation.
            let usage = if error == RequestError::MissingSelector {
                ""
            } else {
                &stderr[..stderr.rfind("clean.py: error:").unwrap()]
            };
            assert_eq!(
                render_request_error(&error, "clean.py", usage),
                CommandOutput {
                    stdout,
                    stderr,
                    exit_code
                }
            );
        }
    }
}
