//! Exact-head observations for deterministic graph gates.
use super::{Check, encode, mark_conflict, required_checks, summarise_ci};
use crate::pr::observer::{Check as Observed, ObserveError, Observer};
use provenant_fabric_runtime::graph::{
    CiFailedCheck, CiObservation, CiState, ConflictObservation, ConflictState,
};
use serde::Deserialize;
use std::{path::Path, time::Duration};

/// The longest a graph gate's one observation may spend reading checks.
const READ_BUDGET: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Head {
    repository: String,
    sha: String,
}

pub(super) fn observe(
    state: &Path,
    project: &Path,
    target: &str,
    names: &[String],
    delivery_queue: bool,
    failed_checks_max_bytes: usize,
    max_failed_checks: usize,
) -> CiObservation {
    let read = |args: &[&str]| {
        super::super::successful(
            super::super::forge_execute(
                state,
                project,
                args,
                &provenant_config::ports::Request::within(READ_BUDGET),
                false,
                None,
            )
            .ok()?,
        )
        .ok()
    };
    let rate_limit = std::cell::RefCell::new(None);
    let evidence = |head: &Head| {
        let request = provenant_config::ports::Request::within(READ_BUDGET);
        let mut observer = Observer::open(state, project, &head.repository, &request).ok()?;
        let max_age = observer.fast_interval();
        match observer.checks(&head.sha, max_age, &request) {
            Ok(checks) => Some(checks),
            Err(ObserveError::RateLimited { reset }) => {
                *rate_limit.borrow_mut() = Some(reset);
                None
            }
            Err(_) => None,
        }
    };
    let mut result = observe_with(target, names, delivery_queue, &read, evidence, |head| {
        required_checks(state, project, &policy_target(target, head, &read)?)
    });
    if let Some(reset) = rate_limit.into_inner() {
        wait_for_rate_limit(&mut result, reset);
    }
    result.bound_failed_checks(failed_checks_max_bytes, max_failed_checks);
    result
}

/// Quota is a retryable observation, not a CI verdict. The shared observer
/// suppresses further forge reads until the recorded reset.
fn wait_for_rate_limit(result: &mut CiObservation, reset: jiff::Timestamp) {
    result.state = CiState::Pending;
    result.summary = format!("forge rate limit; waiting until {reset}");
    if let Ok(reset) = provenant_core::WallTime::from_micros(reset.as_microsecond()) {
        result.set_quota_reset(reset);
    }
    result.failed_checks.clear();
}

/// Read mergeability before CI so a conflict is routed to merge the base immediately.
pub(super) fn observe_conflict(state: &Path, project: &Path, target: &str) -> ConflictObservation {
    let read = |args: &[&str]| {
        super::super::successful(
            super::super::forge_execute(
                state,
                project,
                args,
                &provenant_config::ports::Request::within(READ_BUDGET),
                false,
                None,
            )
            .ok()?,
        )
        .ok()
    };
    observe_conflict_with(target, &read)
}

fn observe_conflict_with(
    target: &str,
    read: &dyn Fn(&[&str]) -> Option<Vec<u8>>,
) -> ConflictObservation {
    let unavailable = || ConflictObservation {
        state: ConflictState::Unavailable,
        head_sha: None,
        summary: "mergeability observation unavailable".into(),
    };
    let Some(before) = resolve_head(target, read) else {
        return unavailable();
    };
    let Some(mergeability) = mergeable(target, &before, read) else {
        return ConflictObservation {
            head_sha: Some(before.sha),
            summary: "mergeability observation unavailable".into(),
            ..unavailable()
        };
    };
    let Some(after) = resolve_head(target, read) else {
        return ConflictObservation {
            head_sha: Some(before.sha),
            summary: "target head unavailable after reading mergeability".into(),
            ..unavailable()
        };
    };
    if after != before {
        return ConflictObservation {
            state: ConflictState::Pending,
            summary: format!(
                "head superseded {} -> {}; waiting for mergeability",
                before.sha, after.sha
            ),
            head_sha: Some(after.sha),
        };
    }
    let (state, summary) = match mergeability.as_str() {
        "MERGEABLE" => (ConflictState::Clear, "pull request is mergeable"),
        "CONFLICTING" => (
            ConflictState::Conflicting,
            "pull request conflicts with its base",
        ),
        "UNKNOWN" => (ConflictState::Pending, "mergeability is still unknown"),
        _ => return unavailable(),
    };
    ConflictObservation {
        state,
        head_sha: Some(before.sha),
        summary: summary.into(),
    }
}

fn policy_target(
    target: &str,
    head: &Head,
    read: &dyn Fn(&[&str]) -> Option<Vec<u8>>,
) -> Option<String> {
    let Some(branch) = target.strip_prefix("refs/heads/") else {
        return Some(target.to_owned());
    };
    #[derive(Deserialize)]
    struct Pull {
        url: String,
        #[serde(rename = "headRefOid")]
        sha: String,
        #[serde(rename = "headRepository")]
        repository: Option<Repository>,
        #[serde(rename = "headRepositoryOwner")]
        owner: Option<Owner>,
    }
    #[derive(Deserialize)]
    struct Repository {
        name: String,
    }
    #[derive(Deserialize)]
    struct Owner {
        login: String,
    }
    // A delivered branch may be numeric. Find its PR by head explicitly so
    // required-policy lookup cannot reinterpret the branch as a PR number.
    let pulls: Vec<Pull> = serde_json::from_slice(&read(&[
        "gh",
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "open",
        "--json",
        "url,headRepository,headRepositoryOwner,headRefOid",
    ])?)
    .ok()?;
    // A fork can have an identically named branch. Its PR policy does not
    // describe the branch whose exact-head checks this observation queried.
    let matching: Vec<_> = pulls
        .iter()
        .filter(|pull| {
            pull.sha == head.sha
                && pull
                    .owner
                    .as_ref()
                    .zip(pull.repository.as_ref())
                    .is_some_and(|(owner, repo)| {
                        format!("{}/{}", owner.login, repo.name) == head.repository
                    })
        })
        .collect();
    match matching.as_slice() {
        [pull] => Some(pull.url.clone()),
        _ => None,
    }
}

fn observe_with(
    target: &str,
    names: &[String],
    delivery_queue: bool,
    read: &dyn Fn(&[&str]) -> Option<Vec<u8>>,
    evidence: impl FnOnce(&Head) -> Option<Vec<Observed>>,
    policy: impl FnOnce(&Head) -> Option<Vec<String>>,
) -> CiObservation {
    let Some(before) = resolve_head(target, read) else {
        return summarise_ci(None, None);
    };
    let resolved = names.is_empty().then(|| policy(&before));
    let policy_unavailable =
        delivery_queue && names.is_empty() && resolved.as_ref().is_some_and(Option::is_none);
    let required = if names.is_empty() {
        resolved
            .flatten()
            .filter(|names| !names.is_empty())
            .map(|names| ("branch-required", names))
    } else {
        Some(("configured", names.to_vec()))
    };
    let evidence = evidence(&before);
    let mut result = summarise_ci(
        evidence.as_ref().map(|checks| {
            checks
                .iter()
                .map(|evidence| Check {
                    name: evidence.name.clone(),
                    bucket: evidence.bucket.clone(),
                })
                .collect()
        }),
        required
            .as_ref()
            .map(|(policy, names)| (*policy, names.as_slice())),
    );
    if policy_unavailable {
        result.state = CiState::Pending;
        result.summary = "policy=required-unavailable; waiting for the branch check policy".into();
        result.failed_checks.clear();
    }
    // A fresh head may not have registered its checks yet. Wait for them;
    // the gate timeout reports a head whose checks never registered.
    if matches!(result.state, CiState::Unavailable | CiState::NotScheduled)
        && evidence.as_ref().is_some_and(|evidence| {
            let selected: Vec<_> = evidence
                .iter()
                .filter(|evidence| {
                    required
                        .as_ref()
                        .is_none_or(|(_, names)| names.contains(&evidence.name))
                })
                .collect();
            let missing = required.as_ref().is_some_and(|(_, names)| {
                names
                    .iter()
                    .any(|name| !selected.iter().any(|evidence| &evidence.name == name))
            });
            (selected.is_empty() || missing)
                && !selected
                    .iter()
                    .any(|evidence| evidence.bucket == "skipping")
        })
    {
        result.state = CiState::Pending;
        let next = result
            .summary
            .find(" next:")
            .map(|at| result.summary.split_off(at))
            .unwrap_or_default();
        result.summary.push_str(" awaiting check registration");
        result.summary.push_str(&next);
    }
    result.head_sha = Some(before.sha.clone());
    if let Some(evidence) = evidence.filter(|_| !policy_unavailable) {
        result.failed_checks = evidence
            .into_iter()
            .filter(|evidence| {
                matches!(evidence.bucket.as_str(), "fail" | "cancel")
                    && required
                        .as_ref()
                        .is_none_or(|(_, names)| names.contains(&evidence.name))
            })
            .map(|evidence| CiFailedCheck {
                extra: Default::default(),
                job_id: evidence.job_id,
                name: evidence.name,
                url: evidence.url,
                excerpt: evidence.excerpt,
            })
            .collect();
    }
    mark_conflict(&mut result, || mergeable(target, &before, read));
    // Checks and the mutable branch are separate API reads. Never let a green
    // result for the previous push settle the gate after the branch has moved.
    match resolve_head(target, read) {
        Some(after) if after == before => result,
        Some(after) => CiObservation {
            quota_reset_at: None,
            state: CiState::Pending,
            summary: format!(
                "head superseded {} -> {}; waiting for checks",
                before.sha, after.sha
            ),
            head_sha: Some(after.sha),
            failed_checks: Vec::new(),
            check_names: Vec::new(),
            truncated: false,
            extra: Default::default(),
        },
        None => CiObservation {
            quota_reset_at: None,
            state: CiState::Unavailable,
            summary: "target head unavailable after reading checks".into(),
            head_sha: Some(before.sha),
            failed_checks: Vec::new(),
            check_names: Vec::new(),
            truncated: false,
            extra: Default::default(),
        },
    }
}

/// `gh pr view` mergeable state: `MERGEABLE`, `CONFLICTING` or `UNKNOWN`.
/// A node branch reads the PR that carries its exact head, as policy does.
fn mergeable(
    target: &str,
    head: &Head,
    read: &dyn Fn(&[&str]) -> Option<Vec<u8>>,
) -> Option<String> {
    #[derive(Deserialize)]
    struct Merge {
        mergeable: String,
    }
    let pull = policy_target(target, head, read)?;
    serde_json::from_slice::<Merge>(&read(&["gh", "pr", "view", &pull, "--json", "mergeable"])?)
        .ok()
        .map(|merge| merge.mergeable)
}

fn resolve_head(target: &str, read: &dyn Fn(&[&str]) -> Option<Vec<u8>>) -> Option<Head> {
    #[derive(Deserialize)]
    struct Pull {
        #[serde(rename = "headRefOid")]
        sha: String,
        #[serde(rename = "headRepository")]
        repository: Option<Repository>,
        #[serde(rename = "headRepositoryOwner")]
        owner: Option<Owner>,
    }
    #[derive(Deserialize)]
    struct Repository {
        name: String,
    }
    #[derive(Deserialize)]
    struct Owner {
        login: String,
    }
    if !target.starts_with("refs/heads/")
        && let Some(bytes) = read(&[
            "gh",
            "pr",
            "view",
            target,
            "--json",
            "headRefOid,headRepository,headRepositoryOwner",
        ])
    {
        let pull: Pull = serde_json::from_slice(&bytes).ok()?;
        let repository = format!("{}/{}", pull.owner?.login, pull.repository?.name);
        return valid_head(repository, pull.sha);
    }
    // Push delivery need not have a PR yet. Resolve the actual remote branch,
    // rather than trusting a possibly stale local tracking ref.
    #[derive(Deserialize)]
    struct Repo {
        #[serde(rename = "nameWithOwner")]
        name: String,
    }
    let repo: Repo =
        serde_json::from_slice(&read(&["gh", "repo", "view", "--json", "nameWithOwner"])?).ok()?;
    let branch = target
        .strip_prefix("refs/heads/")
        .or_else(|| target.strip_prefix("origin/"))
        .unwrap_or(target);
    let endpoint = format!("repos/{}/branches/{}", repo.name, encode(branch));
    let value: serde_json::Value =
        serde_json::from_slice(&read(&["gh", "api", &endpoint])?).ok()?;
    valid_head(repo.name, value.get("commit")?.get("sha")?.as_str()?.into())
}

fn valid_head(repository: String, sha: String) -> Option<Head> {
    let parts: Vec<_> = repository.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| part.is_empty())
        || sha.len() != 40
        || !sha.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    Some(Head { repository, sha })
}

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The worktree driver includes this module without libtest.
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::{Cell, RefCell};

    const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NEW: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn pull(sha: &str) -> serde_json::Value {
        json!({"headRefOid":sha,"headRepository":{"name":"repo"},"headRepositoryOwner":{"login":"owner"}})
    }

    /// The observer's reading of whatever check pages the fixture serves.
    fn evidence(read: &dyn Fn(&[&str]) -> Option<Vec<u8>>, head: &Head) -> Option<Vec<Observed>> {
        let root = format!("repos/{}/commits/{}", head.repository, head.sha);
        let page = |suffix: &str| -> Option<Vec<serde_json::Value>> {
            serde_json::from_slice(&read(&["gh", "api", &format!("{root}/{suffix}")])?).ok()
        };
        crate::pr::observer::parse_pages(&page("check-runs")?, &page("statuses")?, &head.sha)
    }

    fn checks(sha: &str, status: &str, conclusion: Option<&str>) -> serde_json::Value {
        json!([{"check_runs":[{
            "id":1234,"name":"build","head_sha":sha,"status":status,"conclusion":conclusion,
            "details_url":"https://github.com/owner/repo/actions/runs/1",
            "output":{"title":"Build failed","summary":"error: expected a value","text":"src/main.rs:12"}
        }]}])
    }

    #[test]
    fn rate_limit_is_a_wait_with_its_reset_not_a_ci_verdict() {
        let mut observation = summarise_ci(None, None);
        observation.head_sha = Some(OLD.into());
        let reset = jiff::Timestamp::from_second(2_000_000_000).unwrap();
        wait_for_rate_limit(&mut observation, reset);
        assert_eq!(observation.state, CiState::Pending);
        assert_eq!(observation.head_sha.as_deref(), Some(OLD));
        assert!(observation.summary.contains(&reset.to_string()));
        assert_eq!(
            observation.quota_reset_at,
            Some(provenant_core::WallTime::from_micros(reset.as_microsecond()).unwrap())
        );
        assert!(observation.failed_checks.is_empty());
    }

    #[test]
    fn ci_probe_queries_the_exact_head_and_exports_failed_checks() {
        let calls = RefCell::new(Vec::new());
        let read = |args: &[&str]| {
            calls.borrow_mut().push(args.join(" "));
            let value = if args[1] == "pr" {
                pull(OLD)
            } else if args.last()?.contains("check-runs") {
                checks(OLD, "completed", Some("failure"))
            } else if args.last()?.contains("statuses") {
                json!([[]])
            } else {
                return None;
            };
            serde_json::to_vec(&value).ok()
        };
        let observation = observe_with(
            "feature",
            &["build".into()],
            false,
            &read,
            |head| evidence(&read, head),
            |_| None,
        );
        assert_eq!(observation.state, CiState::Failed);
        assert_eq!(observation.head_sha.as_deref(), Some(OLD));
        assert_eq!(observation.check_names, ["build"]);
        assert_eq!(observation.failed_checks.len(), 1);
        assert_eq!(observation.failed_checks[0].name, "build");
        assert_eq!(observation.failed_checks[0].job_id.as_deref(), Some("1234"));
        assert!(
            observation.failed_checks[0]
                .url
                .as_deref()
                .unwrap()
                .ends_with("/runs/1")
        );
        assert!(
            observation.failed_checks[0]
                .excerpt
                .contains("src/main.rs:12")
        );
        assert!(
            calls
                .borrow()
                .iter()
                .any(|call| call.contains(&format!("commits/{OLD}/check-runs")))
        );
        assert!(
            !calls
                .borrow()
                .iter()
                .any(|call| call.starts_with("gh pr checks"))
        );
    }

    #[test]
    fn ci_probe_discards_a_green_result_when_the_head_moves() {
        let reads = Cell::new(0);
        let read = |args: &[&str]| {
            let value = if args[1] == "pr" {
                let count = reads.get();
                reads.set(count + 1);
                pull(if count == 0 { OLD } else { NEW })
            } else if args.last()?.contains("check-runs") {
                checks(OLD, "completed", Some("success"))
            } else {
                json!([[]])
            };
            serde_json::to_vec(&value).ok()
        };
        let observation = observe_with(
            "feature",
            &["build".into()],
            false,
            &read,
            |head| evidence(&read, head),
            |_| None,
        );
        assert_eq!(observation.state, CiState::Pending);
        assert_eq!(observation.head_sha.as_deref(), Some(NEW));
        assert!(observation.summary.contains("superseded"));
        assert!(observation.failed_checks.is_empty());
    }

    #[test]
    fn ci_probe_does_not_accept_stale_check_run_heads_or_incomplete_results() {
        for (sha, status, conclusion, expected) in [
            (NEW, "completed", Some("success"), CiState::Unavailable),
            (OLD, "in_progress", Some("success"), CiState::Pending),
            (OLD, "completed", None, CiState::Unavailable),
            (OLD, "completed", Some("success"), CiState::Passed),
        ] {
            let read = |args: &[&str]| {
                serde_json::to_vec(&if args[1] == "pr" {
                    pull(OLD)
                } else if args.last()?.contains("check-runs") {
                    checks(sha, status, conclusion)
                } else {
                    json!([[]])
                })
                .ok()
            };
            let observation = observe_with(
                "feature",
                &["build".into()],
                false,
                &read,
                |head| evidence(&read, head),
                |_| None,
            );
            assert_eq!(observation.state, expected, "{sha} {status} {conclusion:?}");
            assert!(observation.failed_checks.is_empty());
        }
    }

    #[test]
    fn ci_probe_can_resolve_a_pushed_branch_without_a_pr() {
        let calls = RefCell::new(Vec::new());
        let read = |args: &[&str]| {
            calls.borrow_mut().push(args.join(" "));
            if args[1] == "pr" {
                return None;
            }
            let value = if args[1] == "repo" {
                json!({"nameWithOwner":"owner/repo"})
            } else if args.last()?.contains("branches/") {
                json!({"commit":{"sha":OLD}})
            } else if args.last()?.contains("check-runs") {
                checks(OLD, "completed", Some("success"))
            } else {
                json!([[]])
            };
            serde_json::to_vec(&value).ok()
        };
        let observation = observe_with(
            "feature/repair",
            &["build".into()],
            false,
            &read,
            |head| evidence(&read, head),
            |_| None,
        );
        assert_eq!(observation.state, CiState::Passed);
        assert_eq!(observation.head_sha.as_deref(), Some(OLD));
        assert!(
            calls
                .borrow()
                .iter()
                .any(|call| call.contains("branches/feature%2Frepair"))
        );
    }

    #[test]
    fn queue_ci_waits_when_required_policy_is_unreadable_but_accepts_known_empty_policy() {
        let read = |args: &[&str]| {
            let value = if args[1] == "pr" {
                pull(OLD)
            } else if args.last()?.contains("check-runs") {
                checks(OLD, "completed", Some("success"))
            } else {
                json!([[]])
            };
            serde_json::to_vec(&value).ok()
        };
        let unavailable = observe_with(
            "feature",
            &[],
            true,
            &read,
            |head| evidence(&read, head),
            |_| None,
        );
        assert_eq!(unavailable.state, CiState::Pending);
        assert_eq!(unavailable.head_sha.as_deref(), Some(OLD));
        assert!(unavailable.failed_checks.is_empty());
        assert!(unavailable.summary.contains("policy=required-unavailable"));

        let known_empty = observe_with(
            "feature",
            &[],
            true,
            &read,
            |head| evidence(&read, head),
            |_| Some(Vec::new()),
        );
        assert_eq!(known_empty.state, CiState::Passed);
        assert_eq!(known_empty.head_sha.as_deref(), Some(OLD));
        assert!(known_empty.summary.contains("policy=all-reported"));
    }

    #[test]
    fn ci_probe_keeps_a_numeric_node_branch_distinct_from_a_pr_number() {
        let calls = RefCell::new(Vec::new());
        let read = |args: &[&str]| {
            calls.borrow_mut().push(args.join(" "));
            let value = if args[1] == "repo" {
                json!({"nameWithOwner":"owner/repo"})
            } else if args.last()?.ends_with("branches/123") {
                json!({"commit":{"sha":OLD}})
            } else if args.last()?.contains("check-runs") {
                checks(OLD, "completed", Some("success"))
            } else if args.last()?.contains("statuses") {
                json!([[]])
            } else {
                return None;
            };
            serde_json::to_vec(&value).ok()
        };
        let observation = observe_with(
            "refs/heads/123",
            &["build".into()],
            false,
            &read,
            |head| evidence(&read, head),
            |_| None,
        );
        assert_eq!(observation.state, CiState::Passed);
        assert_eq!(observation.head_sha.as_deref(), Some(OLD));
        assert!(
            calls
                .borrow()
                .iter()
                .any(|call| call.ends_with("branches/123"))
        );
        assert!(
            !calls
                .borrow()
                .iter()
                .any(|call| call.starts_with("gh pr view"))
        );
    }

    #[test]
    fn ci_probe_waits_for_fresh_checks_to_register_without_accepting_skips() {
        for (check_pages, expected) in [
            (json!([{"check_runs":[]}]), CiState::Pending),
            (checks(OLD, "completed", Some("success")), CiState::Pending),
            (
                checks(OLD, "completed", Some("skipped")),
                CiState::Unavailable,
            ),
            (checks(OLD, "completed", None), CiState::Unavailable),
        ] {
            let read = |args: &[&str]| {
                serde_json::to_vec(&if args[1] == "pr" {
                    pull(OLD)
                } else if args.last()?.contains("check-runs") {
                    check_pages.clone()
                } else {
                    json!([[]])
                })
                .ok()
            };
            let observation = observe_with(
                "feature",
                &["build".into(), "lint".into()],
                false,
                &read,
                |head| evidence(&read, head),
                |_| None,
            );
            assert_eq!(observation.state, expected, "{check_pages}");
            if expected == CiState::Pending {
                assert!(observation.summary.contains("awaiting check registration"));
            }
        }
    }

    #[test]
    fn ci_probe_reports_a_conflicting_pr_before_using_check_results() {
        for (check_pages, mergeable, expected) in [
            (json!([{"check_runs":[]}]), "CONFLICTING", CiState::Conflict),
            (json!([{"check_runs":[]}]), "MERGEABLE", CiState::Pending),
            (
                checks(OLD, "completed", Some("failure")),
                "CONFLICTING",
                CiState::Conflict,
            ),
        ] {
            let read = |args: &[&str]| {
                serde_json::to_vec(&if args.ends_with(&["--json", "mergeable"]) {
                    json!({"mergeable":mergeable})
                } else if args[1] == "pr" {
                    pull(OLD)
                } else if args.last()?.contains("check-runs") {
                    check_pages.clone()
                } else {
                    json!([[]])
                })
                .ok()
            };
            let observation = observe_with(
                "feature",
                &[],
                false,
                &read,
                |head| evidence(&read, head),
                |_| None,
            );
            assert_eq!(observation.state, expected, "{check_pages} {mergeable}");
            assert_eq!(observation.head_sha.as_deref(), Some(OLD));
            match expected {
                CiState::Conflict => assert!(observation.summary.contains("next: merge the base into the branch; never rebase pushed history")),
                CiState::Pending => assert!(observation.summary.ends_with(
                    "awaiting check registration next: check the workflow triggers or push an empty commit to start CI"
                )),
                _ => {}
            }
        }
    }

    #[test]
    fn conflict_probe_maps_unknown_to_pending_and_binds_the_read_head() {
        for (mergeability, expected) in [
            ("CONFLICTING", ConflictState::Conflicting),
            ("MERGEABLE", ConflictState::Clear),
            ("UNKNOWN", ConflictState::Pending),
        ] {
            let reads = RefCell::new(Vec::new());
            let read = |args: &[&str]| {
                reads.borrow_mut().push(args.join(" "));
                let value = if args
                    .ends_with(&["--json", "headRefOid,headRepository,headRepositoryOwner"])
                {
                    pull(OLD)
                } else if args.ends_with(&["--json", "mergeable"]) {
                    json!({"mergeable":mergeability})
                } else {
                    return None;
                };
                serde_json::to_vec(&value).ok()
            };
            let observation = observe_conflict_with("123", &read);
            assert_eq!(observation.state, expected);
            assert_eq!(observation.head_sha.as_deref(), Some(OLD));
            assert!(
                !reads
                    .borrow()
                    .iter()
                    .any(|call| call.contains("check-runs"))
            );
        }
    }

    #[test]
    fn conflict_probe_waits_for_mergeability_on_a_moved_head() {
        let heads = Cell::new(0);
        let read = |args: &[&str]| {
            let value =
                if args.ends_with(&["--json", "headRefOid,headRepository,headRepositoryOwner"]) {
                    let count = heads.get();
                    heads.set(count + 1);
                    pull(if count == 0 { OLD } else { NEW })
                } else if args.ends_with(&["--json", "mergeable"]) {
                    json!({"mergeable":"CONFLICTING"})
                } else {
                    return None;
                };
            serde_json::to_vec(&value).ok()
        };
        let moved = observe_conflict_with("123", &read);
        assert_eq!(moved.state, ConflictState::Pending);
        assert_eq!(moved.head_sha.as_deref(), Some(NEW));
        assert!(moved.summary.contains(OLD) && moved.summary.contains(NEW));
    }

    #[test]
    fn node_branch_policy_is_looked_up_by_its_pr_url() {
        let head = Head {
            repository: "owner/repo".into(),
            sha: OLD.into(),
        };
        let url = "https://github.com/owner/repo/pull/456";
        let calls = RefCell::new(Vec::new());
        let read = |args: &[&str]| {
            calls.borrow_mut().push(args.join(" "));
            let mut value = pull(OLD);
            value["url"] = json!(url);
            serde_json::to_vec(&json!([value])).ok()
        };
        assert_eq!(
            policy_target("refs/heads/123", &head, &read).as_deref(),
            Some(url)
        );
        assert_eq!(
            calls.borrow().as_slice(),
            [
                "gh pr list --head 123 --state open --json url,headRepository,headRepositoryOwner,headRefOid"
            ]
        );
        assert_eq!(policy_target("123", &head, &read).as_deref(), Some("123"));
        assert_eq!(calls.borrow().len(), 1);
    }

    #[test]
    fn node_branch_policy_falls_back_when_a_pr_is_absent_ambiguous_or_unreadable() {
        let head = Head {
            repository: "owner/repo".into(),
            sha: OLD.into(),
        };
        let mut first = pull(OLD);
        first["url"] = json!("https://github.com/owner/repo/pull/1");
        let mut second = pull(OLD);
        second["url"] = json!("https://github.com/owner/repo/pull/2");
        for response in [
            json!([]),
            json!([first, second]),
            json!({"error":"unavailable"}),
        ] {
            assert_eq!(
                policy_target("refs/heads/123", &head, &|_| serde_json::to_vec(&response)
                    .ok()),
                None
            );
        }
        assert_eq!(policy_target("refs/heads/123", &head, &|_| None), None);
    }

    #[test]
    fn node_branch_policy_rejects_an_identically_named_fork_branch_or_stale_head() {
        let head = Head {
            repository: "owner/repo".into(),
            sha: OLD.into(),
        };
        for (owner, repository, sha) in [
            ("fork-owner", "repo", OLD),
            ("owner", "other-repo", OLD),
            ("owner", "repo", NEW),
        ] {
            let response = json!([{
                "url":"https://github.com/owner/repo/pull/1", "headRefOid":sha,
                "headRepository":{"name":repository}, "headRepositoryOwner":{"login":owner}
            }]);
            assert_eq!(
                policy_target("refs/heads/123", &head, &|_| serde_json::to_vec(&response)
                    .ok()),
                None
            );
        }
    }
}
