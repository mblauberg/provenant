//! Attempt ancestry contract shared by run owners and provider boundaries.
//!
//! This unit defines the interface, not the journal/cancellation implementation.
//! Process identities and lock custody must come from the supervisor's safe API.
pub use crate::pending::OwnerCustody;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "serde_json::Value")]
pub struct ParentAttempt {
    run_id: String,
    task_id: String,
    attempt: u64,
    run_dir: std::path::PathBuf,
}

impl TryFrom<serde_json::Value> for ParentAttempt {
    type Error = &'static str;
    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        let run_id = value
            .get("run_id")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("invalid parent run id")?;
        let task_id = value
            .get("task_id")
            .and_then(serde_json::Value::as_str)
            .filter(|s| {
                let bytes = s.as_bytes();
                !bytes.is_empty()
                    && bytes.len() <= 64
                    && bytes[0].is_ascii_alphanumeric()
                    && bytes
                        .iter()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
            })
            .ok_or("invalid parent task id")?;
        let attempt = value
            .get("attempt")
            .and_then(serde_json::Value::as_u64)
            .filter(|n| *n > 0)
            .ok_or("invalid parent attempt")?;
        let run_dir = value
            .get("run_dir")
            .and_then(serde_json::Value::as_str)
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or("invalid parent run directory")?;
        Ok(Self {
            run_id: run_id.into(),
            task_id: task_id.into(),
            attempt,
            run_dir,
        })
    }
}

impl ParentAttempt {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
    pub fn attempt(&self) -> u64 {
        self.attempt
    }
    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChildReport {
    pub run_id: String,
    #[serde(flatten)]
    pub fields: std::collections::BTreeMap<String, serde_json::Value>,
}

use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const PARENT_ENV: &str = "PROVENANT_PARENT_ATTEMPT";
pub const PARENT_TOKEN_ENV: &str = "PROVENANT_PARENT_RUN_TOKEN";
pub const CUSTODY_ENV: &str = "PROVENANT_NESTED_CUSTODY_FD";

/// Invalid inherited ancestry is ignored, as it is by the Python owner.
pub fn parent_attempt(environment_value: Option<&str>) -> Option<ParentAttempt> {
    environment_value
        .and_then(|s| provenant_records::checked_json::from_str(s).ok())
        .and_then(|v| serde_json::from_value(v).ok())
}

pub fn journal(parent: &ParentAttempt) -> PathBuf {
    PathBuf::from(format!(
        "dispatch/tasks/{}/attempt-{:03}/tmp/children",
        parent.task_id(),
        parent.attempt()
    ))
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("nested parent run token is missing or does not match")]
    ParentToken,
    #[error("nested parent attempt is not running or its identity changed")]
    ParentAttempt,
    #[error("nested run custody identity changed")]
    CustodyIdentity,
    #[error("batch run custody is not held")]
    CustodyNotHeld,
    #[error("unverified child owner")]
    ChildOwner,
    #[error("unverified child parent")]
    ChildParent,
    #[error("invalid child run directory")]
    ChildDirectory,
    #[error("run lineage record must be an object")]
    Record,
    #[error("run lineage JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Check both proofs before a caller writes either side of the ancestry link.
/// The journal implementation must repeat this check while holding journal custody.
pub fn check_parent(
    parent: &ParentAttempt,
    inherited_token: Option<&str>,
    owner: &serde_json::Value,
    attempt: &serde_json::Value,
) -> Result<(), Error> {
    if inherited_token.filter(|s| !s.is_empty())
        != owner.get("run_token").and_then(serde_json::Value::as_str)
        || inherited_token.is_none_or(str::is_empty)
    {
        return Err(Error::ParentToken);
    }
    if attempt.get("run_id").and_then(serde_json::Value::as_str) != Some(parent.run_id())
        || attempt.get("task_id").and_then(serde_json::Value::as_str) != Some(parent.task_id())
        || !python_numeric_equal(attempt.get("attempt"), parent.attempt())
        || attempt.get("state").and_then(serde_json::Value::as_str) != Some("running")
    {
        return Err(Error::ParentAttempt);
    }
    Ok(())
}

/// All file operations are contained regular-file operations, limited to 1 MiB.
/// Local children must be canonical direct children of `.agent-run/runs`, with
/// matching owner token and receipt ancestry, before completion evidence is read.
/// Remote registration writes only the local receipt; the source host owns its journal.
/// No provider-written PID, token or journal grants cancellation authority.
pub trait Lineage {
    type Custody: OwnerCustody;

    /// Validate inherited lock identity before re-exec or borrowed batch custody.
    fn ensure_nested_owner(
        &mut self,
        run_dir: &Path,
        kind: RunKind,
        batch_child: bool,
    ) -> Result<OwnerMode, Error>;

    fn acquire_owner_custody(&mut self, run_dir: &Path) -> Result<Self::Custody, Error>;
    fn record_owner(
        &mut self,
        custody: &Self::Custody,
        run_dir: &Path,
        kind: RunKind,
    ) -> Result<(), Error>;
    fn register_run(
        &mut self,
        run_dir: &Path,
        run_id: &str,
        parent: &ParentAttempt,
        remote: bool,
    ) -> Result<(), Error>;
    fn register_external(
        &mut self,
        parent: &ParentAttempt,
        children: &[ChildReport],
    ) -> Result<(), Error>;
    fn refresh_external(
        &mut self,
        parent: &ParentAttempt,
        observed: &[ChildReport],
    ) -> Result<(), Error>;
    fn children(&mut self, parent: &ParentAttempt) -> Result<Vec<ChildReport>, Error>;
    /// Request cancellation through supervisor-held custody; return retained evidence.
    /// The implementing owner handles explicit deadlines, shared hosts and ancestry.
    fn cancel_children(
        &mut self,
        custody: &Self::Custody,
        parent: &ParentAttempt,
        timeout: std::time::Duration,
    ) -> Result<Vec<ChildReport>, Error>;
}

/// The batch child must retain this decision until its execution custody step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerMode {
    /// Normal direct or nested dispatch; acquire ordinary execution custody.
    Direct,
    /// The batch child is itself the owner and must acquire execution custody.
    StandaloneBatchChild,
    /// A verified batch parent lends custody; do not reacquire its held lock.
    SharedBatchChild,
}

impl OwnerMode {
    pub fn acquire_execution_custody(self) -> bool {
        !matches!(self, Self::SharedBatchChild)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    Dispatch,
    Batch,
}

/// Preserve the error aggregation used by terminal attempt publication.
pub fn child_errors(children: &[ChildReport]) -> Vec<String> {
    children
        .iter()
        .filter_map(|child| {
            let status = child
                .fields
                .get("status")
                .unwrap_or(&serde_json::Value::Null);
            let reaped = child.fields.get("reaped");
            let has_reaped = reaped.is_some_and(provenant_core::py::python_truth);
            if (status.is_null() || matches!(status.as_str(), Some("running" | "ok" | "partial")))
                && !has_reaped
            {
                return None;
            }
            let task = child
                .fields
                .get("task_id")
                .map(python_string)
                .unwrap_or_else(|| "?".into());
            let detail =
                if let Some(reaped) = reaped.filter(|v| provenant_core::py::python_truth(v)) {
                    format!(
                        "{}; reaped {} leftover process(es)",
                        python_string(status),
                        match reaped {
                            serde_json::Value::Array(values) => values.len(),
                            serde_json::Value::Object(values) => values.len(),
                            serde_json::Value::String(value) => value.chars().count(),
                            // Python raises TypeError for len(number/bool). The
                            // interface returns Vec, so preserve truthiness and
                            // report the single truthy primitive as one item.
                            _ => 1,
                        }
                    )
                } else {
                    child
                        .fields
                        .get("error")
                        .filter(|v| provenant_core::py::python_truth(v))
                        .map(python_string)
                        .unwrap_or_else(|| python_string(status))
                };
            Some(format!("child run {} task {task}: {detail}", child.run_id))
        })
        .collect()
}

use provenant_core::py::python_str as python_string;

fn python_numeric_equal(value: Option<&serde_json::Value>, expected: u64) -> bool {
    value.is_some_and(|value| {
        provenant_core::py::python_json_eq(value, &serde_json::Value::from(expected))
    })
}

pub fn annotate_child_errors(error: Option<&str>, children: &[ChildReport]) -> Option<String> {
    let original = error.unwrap_or("");
    let mut messages = Vec::new();
    if !original.is_empty() {
        messages.push(original.to_owned());
    }
    messages.extend(
        child_errors(children)
            .into_iter()
            .filter(|message| !original.contains(message)),
    );
    if messages.is_empty() {
        error.map(str::to_owned)
    } else {
        Some(messages.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oracle::CommandOutput;
    use serde_json::{json, Value};
    fn value() -> Value {
        json!({"run_id":"mcp-parent","task_id":"writer","attempt":1,"run_dir":"/runs/parent"})
    }
    fn parent() -> ParentAttempt {
        serde_json::from_value(value()).unwrap()
    }

    #[test]
    fn parent_requires_explicit_well_formed_identity() {
        assert!(parent_attempt(None).is_none());
        for text in ["null", "[]", "true", "{", "{}"] {
            assert!(parent_attempt(Some(text)).is_none());
        }
        for (key, bad) in [
            ("run_id", json!("")),
            ("task_id", json!("../escape")),
            ("task_id", json!("é")),
            ("task_id", json!("a".repeat(65))),
            ("attempt", json!(true)),
            ("attempt", json!(0)),
            ("attempt", json!(-1)),
            ("attempt", json!(1.0)),
            ("run_dir", json!("relative")),
        ] {
            let mut v = value();
            v[key] = bad;
            assert!(parent_attempt(Some(&v.to_string())).is_none(), "{v}");
        }
        let mut v = value();
        v["extra"] = json!("ignored");
        assert_eq!(
            serde_json::to_value(parent_attempt(Some(&v.to_string())).unwrap()).unwrap(),
            value()
        );
    }
    #[test]
    fn journals_use_only_private_attempt_tmp_with_unbounded_width() {
        assert_eq!(
            journal(&parent()),
            Path::new("dispatch/tasks/writer/attempt-001/tmp/children")
        );
        let mut v = value();
        v["attempt"] = json!(1000);
        assert_eq!(
            journal(&serde_json::from_value(v).unwrap()),
            Path::new("dispatch/tasks/writer/attempt-1000/tmp/children")
        );
    }
    #[test]
    fn registration_requires_token_and_running_parent_before_writing() {
        let parent = parent();
        let owner = json!({"run_token":"token"});
        let mut row = value();
        row["state"] = json!("running");
        for token in [None, Some(""), Some("other")] {
            assert!(matches!(
                check_parent(&parent, token, &owner, &row),
                Err(Error::ParentToken)
            ));
        }
        assert!(check_parent(&parent, Some("token"), &owner, &row).is_ok());
        for equivalent in [json!(true), json!(1.0)] {
            let mut equivalent_row = row.clone();
            equivalent_row["attempt"] = equivalent;
            assert!(check_parent(&parent, Some("token"), &owner, &equivalent_row).is_ok());
        }
        let mut large_parent = value();
        large_parent["attempt"] = json!(9_007_199_254_740_993u64);
        let large_parent: ParentAttempt = serde_json::from_value(large_parent).unwrap();
        let mut rounded_row = row.clone();
        rounded_row["attempt"] = json!(9_007_199_254_740_992.0);
        assert!(matches!(
            check_parent(&large_parent, Some("token"), &owner, &rounded_row),
            Err(Error::ParentAttempt)
        ));
        for (field, bad) in [
            ("state", json!("terminal")),
            ("attempt", json!(2)),
            ("run_id", json!("other")),
            ("task_id", json!("other")),
        ] {
            let mut wrong = row.clone();
            wrong[field] = bad;
            assert!(matches!(
                check_parent(&parent, Some("token"), &owner, &wrong),
                Err(Error::ParentAttempt)
            ));
        }
    }
    #[test]
    fn failed_and_reaped_children_annotate_without_duplicate_errors() {
        let rows: Vec<ChildReport> = serde_json::from_value(json!([
            {"run_id":"ok","status":"ok"},
            {"run_id":"failed","task_id":"writer","status":"failed","error":"boom"},
            {"run_id":"reaped","status":"ok","reaped":[{"pid":3}]},
            {"run_id":"partial","status":"partial"}
        ]))
        .unwrap();
        let errors = child_errors(&rows);
        assert_eq!(
            errors,
            [
                "child run failed task writer: boom",
                "child run reaped task ?: ok; reaped 1 leftover process(es)"
            ]
        );
        let annotated = annotate_child_errors(Some("original"), &rows).unwrap();
        assert_eq!(
            annotate_child_errors(Some(&annotated), &rows),
            Some(annotated)
        );
        assert_eq!(annotate_child_errors(None, &[]), None);
    }
    #[test]
    fn differential_parent_parsing() {
        let script_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../skills/orchestrate/scripts");
        let fixtures = serde_json::to_string(&[
            value().to_string(),
            r#"{"run_id":"r","task_id":"bad/path","attempt":1,"run_dir":"/r"}"#.into(),
            r#"{"run_id":"r","task_id":"t","attempt":true,"run_dir":"/r"}"#.into(),
            r#"{"run_id":"r","task_id":"t","attempt":1.0,"run_dir":"/r"}"#.into(),
            r#"{"run_id":"r","task_id":"t","attempt":18446744073709551616,"run_dir":"/r"}"#.into(),
        ])
        .unwrap();
        let Some(mut python) = crate::oracle::python() else {
            return;
        };
        let result = python
            .args([
                "-c",
                r#"
import json,os,sys
sys.path.insert(0,sys.argv[1]);import run_lineage as m
rows=[]
for raw in json.loads(sys.argv[2]):
 v=json.loads(raw)
 os.environ[m.PARENT_ENV]=json.dumps(v);rows.append(m.parent_attempt())
print(json.dumps(rows))
"#,
            ])
            .arg(script_root)
            .arg(&fixtures)
            .bounded_output();
        let result = match result {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                eprintln!("SKIP: python3 absent");
                return;
            }
            v => v.unwrap(),
        };
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let raw_rows: Vec<String> = serde_json::from_str(&fixtures).unwrap();
        let actual: Vec<_> = raw_rows.iter().map(|v| parent_attempt(Some(v))).collect();
        let mut expected = serde_json::from_slice::<Value>(&result.stdout).unwrap();
        // Python accepts arbitrary-size ints; this typed API stores u64 and
        // treats the out-of-range fixture as an invalid parent attempt.
        expected.as_array_mut().unwrap()[4] = Value::Null;
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    use crate::oracle::CommandOutput;
    use serde_json::{json, Value};
    #[test]
    fn batch_owner_mode_preserves_execution_custody_decision() {
        assert!(OwnerMode::StandaloneBatchChild.acquire_execution_custody());
        assert!(!OwnerMode::SharedBatchChild.acquire_execution_custody());
        assert!(OwnerMode::Direct.acquire_execution_custody());
    }
    #[test]
    fn falsey_json_child_errors_fall_back_to_status() {
        for error in [
            json!(false),
            json!(0),
            json!(0.0),
            json!(""),
            json!([]),
            json!({}),
            json!(null),
        ] {
            let row: ChildReport =
                serde_json::from_value(json!({"run_id":"x","status":"failed","error":error}))
                    .unwrap();
            assert_eq!(child_errors(&[row]), ["child run x task ?: failed"]);
        }
        let row: ChildReport =
            serde_json::from_value(json!({"run_id":"x","status":false})).unwrap();
        assert_eq!(child_errors(&[row]), ["child run x task ?: False"]);
    }

    #[test]
    fn differential_python_repr_and_reaped_truthiness_shapes() {
        let children: Vec<ChildReport> = serde_json::from_value(json!([
            {"run_id":"a","status":"failed","error":["a"]},
            {"run_id":"b","status":"failed","error":{"k":1}},
            {"run_id":"c","status":"failed","error":1e20},
            {"run_id":"d","status":"failed","error":true},
            {"run_id":"e","status":null,"reaped":"abc"},
            {"run_id":"f","status":"ok","reaped":{"k":1,"j":2}},
        ]))
        .unwrap();
        let script = r#"
import json,sys
sys.path.insert(0,sys.argv[1]);import run_lineage as m
print(json.dumps(m.child_errors(json.loads(sys.argv[2]))))
"#;
        let script_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../skills/orchestrate/scripts");
        let Some(mut python) = crate::oracle::python() else {
            return;
        };
        let result = python
            .args([
                "-c",
                script,
                script_root.to_str().unwrap(),
                &serde_json::to_string(&children).unwrap(),
            ])
            .bounded_output();
        let result = match result {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                eprintln!("skip: python3 absent");
                return;
            }
            v => v.unwrap(),
        };
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            serde_json::to_value(child_errors(&children)).unwrap(),
            serde_json::from_slice::<Value>(&result.stdout).unwrap()
        );
        assert_eq!(
            child_errors(&children),
            [
                "child run a task ?: ['a']",
                "child run b task ?: {'k': 1}",
                "child run c task ?: 1e+20",
                "child run d task ?: True",
                "child run e task ?: None; reaped 3 leftover process(es)",
                "child run f task ?: ok; reaped 2 leftover process(es)"
            ]
        );
        for reaped in [json!(1), json!(true)] {
            let row: ChildReport =
                serde_json::from_value(json!({"run_id":"primitive","status":"ok","reaped":reaped}))
                    .unwrap();
            assert_eq!(
                child_errors(&[row]),
                ["child run primitive task ?: ok; reaped 1 leftover process(es)"]
            );
        }
    }
}
