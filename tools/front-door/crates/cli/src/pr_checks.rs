//! Required-check policy. GitHub discovery, polling and command rendering remain
//! with the process/API owners; these functions never contact GitHub.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The source's normalized check row. `link` is retained JSON, because the
/// Python normalizer does not coerce truthy non-string link values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub bucket: String,
    pub link: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Pass,
    Fail,
    Pending,
    Unknown,
}
impl Outcome {
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Fail => 1,
            Self::Pending => 3,
            Self::Unknown => 4,
        }
    }
}

pub fn finished(rows: &[Check]) -> bool {
    rows.iter()
        .all(|row| matches!(row.bucket.as_str(), "pass" | "skipping" | "fail" | "cancel"))
}

/// An unknown required set is distinct from a known empty one. Neither can
/// declare success until the finished name list is stable across observations.
pub fn judge(
    rows: &[Check],
    expected: Option<&BTreeSet<String>>,
    previous: Option<&[String]>,
) -> (Vec<Check>, Outcome) {
    let counted: Vec<_> = rows
        .iter()
        .filter(|row| expected.is_none_or(|set| set.is_empty() || set.contains(&row.name)))
        .cloned()
        .collect();
    if let Some(expected) = expected.filter(|set| !set.is_empty()) {
        if counted.is_empty()
            || !expected
                .iter()
                .all(|name| counted.iter().any(|row| row.name == *name))
            || !finished(&counted)
        {
            return (counted, Outcome::Pending);
        }
    } else {
        let mut names: Vec<_> = counted.iter().map(|row| row.name.clone()).collect();
        names.sort();
        if counted.is_empty() || !finished(&counted) || previous != Some(names.as_slice()) {
            return (counted, Outcome::Pending);
        }
    }
    let outcome = if counted
        .iter()
        .any(|row| matches!(row.bucket.as_str(), "fail" | "cancel"))
    {
        Outcome::Fail
    } else if expected.is_none() {
        Outcome::Unknown
    } else {
        Outcome::Pass
    };
    (counted, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn row(name: &str, bucket: &str) -> Check {
        Check {
            name: name.into(),
            bucket: bucket.into(),
            link: json!(""),
        }
    }

    #[test]
    fn source_policy_never_mistakes_absent_or_unknown_checks_for_success() {
        let required = BTreeSet::from(["build".into(), "release / build".into()]);
        assert_eq!(
            judge(&[row("build", "pass")], Some(&required), None).1,
            Outcome::Pending
        );
        assert_eq!(
            judge(&[row("build", "pass")], None, Some(&["build".into()])).1,
            Outcome::Unknown
        );
        assert_eq!(
            judge(&[row("build", "fail")], None, Some(&["build".into()])).1,
            Outcome::Fail
        );
        assert_eq!(
            judge(&[], Some(&BTreeSet::new()), Some(&[])).1,
            Outcome::Pending
        );
        assert_eq!(
            judge(
                &[row("build", "pass"), row("late", "pending")],
                Some(&BTreeSet::new()),
                Some(&["build".into()])
            )
            .1,
            Outcome::Pending
        );
        let (rows, outcome) = judge(
            &[row("required", "skipping"), row("unrelated", "fail")],
            Some(&BTreeSet::from(["required".into()])),
            None,
        );
        assert_eq!(rows, [row("required", "skipping")]);
        assert_eq!(outcome, Outcome::Pass);
        assert_eq!(
            [
                Outcome::Pass,
                Outcome::Fail,
                Outcome::Pending,
                Outcome::Unknown
            ]
            .map(Outcome::exit_code),
            [0, 1, 3, 4]
        );
    }

    #[test]
    fn exact_required_name_and_settlement_decisions_match_python() {
        let rows = vec![
            vec![],
            vec![row("build", "pass")],
            vec![row("build", "fail")],
            vec![row("build", "pending")],
            vec![row("build", "pass"), row("build", "cancel")],
            vec![row("build", "pass"), row("lint", "skipping")],
            vec![row("build", "pass"), row("release / build", "pass")],
        ];
        let expected: Vec<Option<BTreeSet<String>>> = vec![
            None,
            Some(BTreeSet::new()),
            Some(BTreeSet::from(["build".into()])),
            Some(BTreeSet::from(["build".into(), "lint".into()])),
            Some(BTreeSet::from(["release / build".into()])),
        ];
        let previous: Vec<Option<Vec<String>>> = vec![
            None,
            Some(vec![]),
            Some(vec!["build".into()]),
            Some(vec!["build".into(), "build".into()]),
            Some(vec!["build".into(), "lint".into()]),
        ];
        let mut cases = vec![];
        for rows in &rows {
            for required in &expected {
                for prev in &previous {
                    cases.push((rows.clone(), required.clone(), prev.clone()));
                }
            }
        }
        let Some(oracle):Option<Vec<(Vec<Check>,Outcome)>>=crate::oracle::python(
            "import json,sys; from scripts.provenant_pr import judge; print(json.dumps([judge(rows,None if required is None else set(required),previous) for rows,required,previous in json.load(sys.stdin)]))",&json!(cases)
        ) else {return;};
        assert_eq!(cases.len(), oracle.len());
        for ((rows, expected, previous), oracle) in cases.into_iter().zip(oracle) {
            assert_eq!(judge(&rows, expected.as_ref(), previous.as_deref()), oracle);
        }
    }
}

fn normalize_checks(value: &crate::pending::OrderedValue) -> Option<Vec<Check>> {
    use crate::pending::OrderedValue as O;
    use provenant_core::py::python_truth;
    let O::Array(items) = value else {
        return None;
    };
    Some(
        items
            .iter()
            .filter_map(|row| {
                let O::Object(row) = row else {
                    return None;
                };
                Some(row)
            })
            .map(|row| {
                let get = |key| row.iter().find(|(k, _)| k == key).map(|(_, v)| v);
                let name = get("name").map(O::python_str).unwrap_or_default();
                let bucket = get("bucket")
                    .filter(|v| python_truth(&v.value()))
                    .map(O::python_str)
                    .unwrap_or_else(|| "pending".into());
                let link = get("link")
                    .map(O::value)
                    .filter(python_truth)
                    .unwrap_or_else(|| serde_json::Value::String(String::new()));
                Check { name, bucket, link }
            })
            .collect(),
    )
}
/// Preserve the shared integer range check. Syntax errors are gh's unknown
/// answer; out-of-range integer tokens stay an explicit typed refusal.
pub fn parse_checks(raw: &str) -> Result<Option<Vec<Check>>, provenant_records::DecodeError> {
    match crate::pending::OrderedValue::checked(raw) {
        Ok(value) => Ok(normalize_checks(&value)),
        Err(provenant_records::DecodeError::Json(_)) => Ok(None),
        Err(error) => Err(error),
    }
}
/// No-required-checks fallback policy is independent of subprocess exit code:
/// gh reports a completed failing check set with a nonzero status too.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetch {
    Rows(Vec<Check>),
    AllChecks,
    Unknown,
}
pub fn required_response(
    stdout: &str,
    stderr: &str,
) -> Result<Fetch, provenant_records::DecodeError> {
    if let Some(rows) = parse_checks(stdout)? {
        return Ok(Fetch::Rows(rows));
    }
    let message = stderr.to_lowercase();
    Ok(if message.contains("no required checks") {
        Fetch::AllChecks
    } else if message.contains("no checks reported") {
        Fetch::Rows(vec![])
    } else {
        Fetch::Unknown
    })
}
pub fn all_response(stdout: &str, stderr: &str) -> Result<Fetch, provenant_records::DecodeError> {
    Ok(match parse_checks(stdout)? {
        Some(rows) => Fetch::Rows(rows),
        None if stderr.to_lowercase().contains("no checks reported") => Fetch::Rows(vec![]),
        None => Fetch::Unknown,
    })
}

#[cfg(test)]
mod normalization_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn normalization_matches_python_truthiness_scalar_str_and_retained_links() {
        let cases=["invalid".to_owned(),"{}".into(),"null".into(), r#"[{"name":-0,"bucket":[-0,-0.0]}]"#.into(),"[]".into(), r#"[{"name":{"z":true,"a":null,"z":false},"bucket":[{"z":1,"a":2}]}]"#.into(),serde_json::to_string(&json!([null,false,1,"row",{}, {"name":null,"bucket":0,"link":false},{"name":true,"bucket":1.0,"link":[1]},{"name":9007199254740993u64,"bucket":"pass","link":{"a":null}},{"name":"é","bucket":[],"link":{}}])).unwrap()];
        let Some(expected):Option<Vec<Option<Vec<Check>>>>=crate::oracle::python("import json,sys; from scripts.provenant_pr import parse_checks; print(json.dumps([parse_checks(v) for v in json.load(sys.stdin)]))",&json!(cases)) else{return;};
        for (raw, expected) in cases.iter().zip(expected) {
            assert_eq!(parse_checks(raw).unwrap(), expected);
        }
        assert!(matches!(
            parse_checks("[{\"unused\":18446744073709551616}]"),
            Err(provenant_records::DecodeError::NumberOutOfRange(_))
        ));
    }
    #[test]
    fn fallback_classification_preserves_no_required_vs_no_registered_checks() {
        assert_eq!(
            required_response("bad", "No required checks reported").unwrap(),
            Fetch::AllChecks
        );
        assert_eq!(
            required_response("bad", "No checks reported").unwrap(),
            Fetch::Rows(vec![])
        );
        assert_eq!(
            required_response("[]", "HTTP failure").unwrap(),
            Fetch::Rows(vec![])
        );
        assert_eq!(all_response("bad", "HTTP failure").unwrap(), Fetch::Unknown);
        assert_eq!(
            all_response("bad", "No checks reported").unwrap(),
            Fetch::Rows(vec![])
        );
    }
}
