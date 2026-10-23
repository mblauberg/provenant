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
