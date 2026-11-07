//! Read-only decisions and restart-safe local digests. These records are never
//! publication receipts and cannot authorise a later active worker.
use crate::event_intake::{Intake, reconcile::Reconciler};
use provenant_workflow::reconciler::Key;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fs, io, path::Path};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Review,
    Repair,
    WaitChecks,
    Land,
    Ignore,
    Escalate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub pr: u64,
    pub head: String,
    pub action: Action,
    pub reason: String,
    pub key: Option<Key>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Digest {
    pub day: u64,
    /// Changed decisions, rather than poll iterations or proposed effect count.
    pub transitions: u64,
    pub errors: u64,
    pub latest: Vec<Decision>,
    pub escalations: Vec<Decision>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub repository: String,
    pub policy_digest: String,
    pub mode: String,
    pub boots: u64,
    pub observations: u64,
    pub first_observed_at: Option<u64>,
    pub observed_at: Option<u64>,
    pub effect_calls: u64,
    pub errors: u64,
    pub last_error: Option<String>,
    pub decisions: Vec<Decision>,
    pub intake: crate::event_intake::reconcile::Status,
    /// Admission-to-durable-completion samples, excluding tunnel delivery time.
    /// These cannot establish issue #2846's event-to-current-state p95.
    pub hint_completion_seconds: Vec<u64>,
    pub digest: Digest,
}

fn text<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}
fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Missing, stale, skipped, neutral or superseded checks never satisfy this
/// gate. Among duplicate check names, the greatest forge ID is authoritative.
fn ci_passes(resources: &BTreeMap<String, Value>, head: &str) -> bool {
    let check = resources
        .get("check_runs")
        .and_then(Value::as_array)
        .and_then(|runs| {
            runs.iter()
                .filter(|run| text(run, "/name") == "ci-status")
                .max_by_key(|run| run["id"].as_u64().unwrap_or(0))
        });
    if let Some(check) = check {
        return text(check, "/head_sha") == head
            && text(check, "/status") == "completed"
            && text(check, "/conclusion") == "success";
    }
    // Status contexts are fetched from the immutable head endpoint by intake.
    resources
        .get("status")
        .and_then(Value::as_array)
        .and_then(|statuses| {
            statuses
                .iter()
                .filter(|status| text(status, "/context") == "ci-status")
                .max_by_key(|status| status["id"].as_u64().unwrap_or(0))
        })
        .is_some_and(|status| text(status, "/state") == "success")
}

/// This readiness signal is deliberately weaker than landing authority. A
/// later action adapter must still run the full ladder, terminal acceptance,
/// configured authority and the existing candidate landing gates.
pub fn decide(
    repository: &str,
    policy: &str,
    base_branch: &str,
    pr: u64,
    resources: &BTreeMap<String, Value>,
) -> Decision {
    let pull = resources.get("pull_request").unwrap_or(&Value::Null);
    let head = text(pull, "/head/sha").to_owned();
    let base = text(pull, "/base/sha");
    let mut decision = Decision {
        pr,
        head: head.clone(),
        action: Action::Escalate,
        reason: "invalid_snapshot".into(),
        key: None,
    };
    if !oid(&head) || !oid(base) || pull["number"].as_u64() != Some(pr) {
        return decision;
    }
    decision.key = Some(Key {
        repository: repository.into(),
        pr,
        head: head.clone(),
        policy_digest: policy.into(),
        lens: "panel".into(),
        base_digest: base.into(),
    });
    let (action, reason) = if text(pull, "/state") == "closed" {
        (Action::Ignore, "closed")
    } else if text(pull, "/state") != "open" {
        (Action::Escalate, "unknown_pr_state")
    } else if pull["draft"].as_bool() != Some(false) {
        (Action::Ignore, "draft_or_unknown")
    } else if text(pull, "/base/ref") != base_branch {
        (Action::Ignore, "other_base")
    } else if text(pull, "/head/repo/full_name") != repository
        || text(pull, "/base/repo/full_name") != repository
    {
        (Action::Escalate, "fork_or_unknown_repository")
    } else if pull["labels"].as_array().is_some_and(|labels| {
        labels
            .iter()
            .any(|label| text(label, "/name") == "risk:terminal")
    }) {
        (Action::Escalate, "terminal_owner_acceptance")
    } else {
        let author = text(pull, "/user/login").to_ascii_lowercase();
        let mut latest: BTreeMap<String, &Value> = BTreeMap::new();
        if let Some(reviews) = resources.get("reviews").and_then(Value::as_array) {
            for review in reviews {
                let reviewer = text(review, "/user/login").to_ascii_lowercase();
                if reviewer.is_empty()
                    || reviewer == author
                    || !matches!(
                        text(review, "/state"),
                        "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"
                    )
                {
                    continue;
                }
                let previous = latest
                    .get(&reviewer)
                    .map_or(0, |old| old["id"].as_u64().unwrap_or(0));
                if review["id"].as_u64().is_some_and(|id| id > previous) {
                    latest.insert(reviewer, review);
                }
            }
        }
        if author.is_empty() {
            (Action::Escalate, "unknown_author")
        } else if latest.values().any(|review| {
            text(review, "/commit_id") == head && text(review, "/state") == "CHANGES_REQUESTED"
        }) {
            (Action::Repair, "changes_requested_at_head")
        } else if latest
            .values()
            .filter(|review| {
                text(review, "/commit_id") == head && text(review, "/state") == "APPROVED"
            })
            .count()
            < 2
        {
            (Action::Review, "review_coverage_unverified")
        } else if !ci_passes(resources, &head) {
            (Action::WaitChecks, "exact_head_ci_status_not_successful")
        } else {
            (Action::Land, "would_evaluate_authority_and_candidate_gates")
        }
    };
    decision.action = action;
    decision.reason = reason.into();
    decision
}

/// Local report and digest share one atomic checkpoint. The digest file is a
/// replaceable projection of it, so a crash cannot double-count transitions.
pub struct Observer {
    report: Report,
}
impl Observer {
    pub fn open(directory: &Path, repository: &str, policy: &str) -> io::Result<Self> {
        let report = match fs::read(directory.join("report.json")) {
            Ok(bytes) => {
                let mut report: Report =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                if report.schema_version != 1
                    || report.repository != repository
                    || report.policy_digest != policy
                    || report.mode != "observe"
                    || report.effect_calls != 0
                {
                    return Err(io::Error::other("incompatible observation checkpoint"));
                }
                // The checkpoint commits before its digest projection. Replay
                // that projection before a new day can replace the recovered
                // digest, including when the last process died between writes.
                project_digest(directory, &report.digest)?;
                report.boots = report.boots.saturating_add(1);
                report
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Report {
                schema_version: 1,
                repository: repository.into(),
                policy_digest: policy.into(),
                mode: "observe".into(),
                boots: 1,
                observations: 0,
                first_observed_at: None,
                observed_at: None,
                effect_calls: 0,
                errors: 0,
                last_error: None,
                decisions: vec![],
                intake: Default::default(),
                hint_completion_seconds: vec![],
                digest: Default::default(),
            },
            Err(error) => return Err(error),
        };
        Ok(Self { report })
    }
    pub fn report(&self) -> &Report {
        &self.report
    }
    pub fn observe_completion(&mut self, intake: &Intake, reconciler: &Reconciler) {
        self.report.intake = reconciler.status(intake);
        self.report.hint_completion_seconds = intake
            .markers()
            .values()
            .filter_map(|marker| {
                marker
                    .completed_at
                    .map(|at| at.saturating_sub(marker.received_at))
            })
            .collect();
    }
    fn day(&mut self, now: u64) {
        if self.report.digest.day != now / 86400 {
            self.report.digest = Digest {
                day: now / 86400,
                ..Default::default()
            };
        }
    }
    pub fn observe(&mut self, reconciler: &Reconciler, intake: &Intake, base: &str, now: u64) {
        self.day(now);
        let decisions: Vec<_> = reconciler
            .current()
            .iter()
            .map(|(&pr, resources)| {
                decide(
                    &self.report.repository,
                    &self.report.policy_digest,
                    base,
                    pr,
                    resources,
                )
            })
            .collect();
        for decision in &decisions {
            if !self.report.decisions.contains(decision) {
                self.report.digest.transitions = self.report.digest.transitions.saturating_add(1);
            }
        }
        self.report.observations = self.report.observations.saturating_add(1);
        self.report.first_observed_at.get_or_insert(now);
        self.report.observed_at = Some(now);
        self.report.last_error = None;
        self.report.decisions = decisions.clone();
        self.report.intake = reconciler.status(intake);
        self.report.hint_completion_seconds = intake
            .markers()
            .values()
            .filter_map(|marker| {
                marker
                    .completed_at
                    .map(|at| at.saturating_sub(marker.received_at))
            })
            .collect();
        self.report.digest.escalations = decisions
            .iter()
            .filter(|decision| matches!(decision.action, Action::Escalate | Action::Repair))
            .cloned()
            .collect();
        self.report.digest.latest = decisions;
    }
    pub fn error(&mut self, now: u64) {
        self.day(now);
        self.report.errors = self.report.errors.saturating_add(1);
        self.report.digest.errors = self.report.digest.errors.saturating_add(1);
        // Do not persist forge bodies, credentials or uncontrolled error text.
        self.report.last_error = Some("intake_reconciliation_failed".into());
    }
    pub fn persist(&self, directory: &Path) -> io::Result<()> {
        use provenant_os::fs::publish_marker;
        fs::create_dir_all(directory)?;
        let bytes = serde_json::to_vec_pretty(&self.report).map_err(io::Error::other)?;
        publish_marker(&directory.join("report.json"), &bytes).map_err(io::Error::other)?;
        project_digest(directory, &self.report.digest)
    }
}

fn project_digest(directory: &Path, digest: &Digest) -> io::Result<()> {
    fs::create_dir_all(directory.join("digests"))?;
    let bytes = serde_json::to_vec_pretty(digest).map_err(io::Error::other)?;
    provenant_os::fs::publish_marker(
        &directory
            .join("digests")
            .join(format!("{}.json", digest.day)),
        &bytes,
    )
    .map(|_| ())
    .map_err(io::Error::other)
}

#[cfg(test)]
#[allow(dead_code, unused_imports)] // Also compiled by the harness=false worktree driver, without libtest.
mod tests {
    use super::*;
    use serde_json::json;
    fn resources() -> BTreeMap<String, Value> {
        let head = "a".repeat(40);
        BTreeMap::from([
            (
                "pull_request".into(),
                json!({"number":7,"state":"open","draft":false,"head":{"sha":head,"repo":{"full_name":"o/r"}},"base":{"ref":"main","sha":"b".repeat(40),"repo":{"full_name":"o/r"}},"user":{"login":"author"},"labels":[]}),
            ),
            (
                "reviews".into(),
                json!([
                    {"id":1,"state":"APPROVED","commit_id":head,"user":{"login":"reader1"}},
                    {"id":2,"state":"APPROVED","commit_id":head,"user":{"login":"reader2"}}
                ]),
            ),
            (
                "check_runs".into(),
                json!([{"id":1,"head_sha":head,"name":"ci-status","status":"completed","conclusion":"success"}]),
            ),
        ])
    }
    fn action(resources: &BTreeMap<String, Value>) -> Action {
        decide("o/r", "policy", "main", 7, resources).action
    }
    #[test]
    fn restart_replays_checkpoint_digest_before_day_rollover() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let mut observer = Observer::open(directory.path(), "o/r", "policy")?;
        observer.error(10 * 86400);
        observer.persist(directory.path())?;

        // Model death after the next checkpoint write but before projecting it:
        // day 10's projection still has one error, the checkpoint has two.
        observer.error(10 * 86400 + 1);
        fs::write(
            directory.path().join("report.json"),
            serde_json::to_vec(&observer.report).map_err(io::Error::other)?,
        )?;
        drop(observer);
        let mut restarted = Observer::open(directory.path(), "o/r", "policy")?;
        restarted.error(11 * 86400);
        restarted.persist(directory.path())?;

        let old: Digest =
            serde_json::from_slice(&fs::read(directory.path().join("digests/10.json"))?)
                .map_err(io::Error::other)?;
        let current: Digest =
            serde_json::from_slice(&fs::read(directory.path().join("digests/11.json"))?)
                .map_err(io::Error::other)?;
        assert_eq!((old.day, old.errors), (10, 2));
        assert_eq!((current.day, current.errors), (11, 1));
        assert_eq!(restarted.report().boots, 2);
        assert_eq!(restarted.report().errors, 3);
        Ok(())
    }
    #[test]
    fn landing_readiness_requires_latest_successful_exact_head_ci_status() {
        let good = resources();
        assert_eq!(action(&good), Action::Land);
        for (field, value) in [
            ("head_sha", json!("c".repeat(40))),
            ("head_sha", Value::Null),
            ("name", json!("other")),
            ("status", json!("in_progress")),
            ("conclusion", json!("skipped")),
            ("conclusion", json!("neutral")),
            ("conclusion", json!("failure")),
        ] {
            let mut changed = good.clone();
            changed.get_mut("check_runs").unwrap()[0][field] = value;
            assert_eq!(action(&changed), Action::WaitChecks, "{field}");
        }
        let mut changed = good.clone();
        changed
            .get_mut("check_runs")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(
                json!({"id":2,"head_sha":"a".repeat(40),"name":"ci-status","status":"in_progress"}),
            );
        assert_eq!(action(&changed), Action::WaitChecks);
        changed.remove("check_runs");
        changed.insert(
            "status".into(),
            json!([{"id":3,"context":"ci-status","state":"success"}]),
        );
        assert_eq!(action(&changed), Action::Land);
        changed
            .get_mut("status")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!({"id":4,"context":"ci-status","state":"pending"}));
        assert_eq!(action(&changed), Action::WaitChecks);
    }
    #[test]
    fn reviews_are_per_reviewer_and_exact_head_with_latest_decisive_state() {
        let mut changed = resources();
        let head = "a".repeat(40);
        changed.get_mut("reviews").unwrap().as_array_mut().unwrap().extend([
            json!({"id":3,"state":"CHANGES_REQUESTED","commit_id":head,"user":{"login":"READER1"}}),
            json!({"id":4,"state":"COMMENTED","commit_id":head,"user":{"login":"reader1"}}),
        ]);
        assert_eq!(action(&changed), Action::Repair);
        changed
            .get_mut("reviews")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!({"id":5,"state":"DISMISSED","commit_id":head,"user":{"login":"reader1"}}));
        assert_eq!(action(&changed), Action::Review);
        changed
            .get_mut("reviews")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!({"id":6,"state":"APPROVED","commit_id":head,"user":{"login":"AUTHOR"}}));
        assert_eq!(action(&changed), Action::Review);
        let mut changed = resources();
        changed.get_mut("pull_request").unwrap()["head"]["sha"] = json!("c".repeat(40));
        assert_eq!(action(&changed), Action::Review);
        let decision = decide("o/r", "policy2", "main", 7, &changed);
        assert_eq!(decision.key.unwrap().policy_digest, "policy2");
    }
    #[test]
    fn unsafe_or_inapplicable_snapshots_never_propose_landing() {
        for (pointer, value, expected) in [
            ("/state", json!("closed"), Action::Ignore),
            ("/state", json!("surprise"), Action::Escalate),
            ("/draft", json!(true), Action::Ignore),
            ("/base/ref", json!("other"), Action::Ignore),
            ("/base/sha", Value::Null, Action::Escalate),
            ("/head/repo/full_name", json!("fork/r"), Action::Escalate),
            (
                "/labels",
                json!([{"name":"risk:terminal"}]),
                Action::Escalate,
            ),
            ("/user/login", Value::Null, Action::Escalate),
        ] {
            let mut changed = resources();
            *changed
                .get_mut("pull_request")
                .unwrap()
                .pointer_mut(pointer)
                .unwrap() = value;
            assert_eq!(action(&changed), expected, "{pointer}");
        }
    }
}
