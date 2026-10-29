//! Exact-head observations for deterministic graph gates.
use super::{Check, encode, required_checks, summarise_ci};
use provenant_fabric_runtime::graph::{CiFailedCheck, CiObservation, CiState};
use serde::Deserialize;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Head {
    repository: String,
    sha: String,
}

struct Evidence {
    check: Check,
    url: Option<String>,
    excerpt: String,
}

pub(super) fn observe(
    project: &Path,
    target: &str,
    names: &[String],
    failed_checks_max_bytes: usize,
    max_failed_checks: usize,
) -> CiObservation {
    let read = |args: &[&str]| {
        super::super::successful(super::super::execute(project, args, false).ok()?).ok()
    };
    let mut result = observe_with(target, names, &read, |head| {
        required_checks(project, &policy_target(target, head, &read)?)
    });
    result.bound_failed_checks(failed_checks_max_bytes, max_failed_checks);
    result
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
    read: &dyn Fn(&[&str]) -> Option<Vec<u8>>,
    policy: impl FnOnce(&Head) -> Option<Vec<String>>,
) -> CiObservation {
    let Some(before) = resolve_head(target, read) else {
        return summarise_ci(None, None);
    };
    let required = if names.is_empty() {
        policy(&before).map(|names| ("branch-required", names))
    } else {
        Some(("configured", names.to_vec()))
    };
    let evidence = read_evidence(&before, read);
    let mut result = summarise_ci(
        evidence.as_ref().map(|checks| {
            checks
                .iter()
                .map(|evidence| Check {
                    name: evidence.check.name.clone(),
                    bucket: evidence.check.bucket.clone(),
                })
                .collect()
        }),
        required
            .as_ref()
            .map(|(policy, names)| (*policy, names.as_slice())),
    );
    if result.state == CiState::Unavailable
        && evidence.as_ref().is_some_and(|evidence| {
            let selected: Vec<_> = evidence
                .iter()
                .filter(|evidence| {
                    required
                        .as_ref()
                        .is_none_or(|(_, names)| names.contains(&evidence.check.name))
                })
                .collect();
            let missing = required.as_ref().is_some_and(|(_, names)| {
                names
                    .iter()
                    .any(|name| !selected.iter().any(|evidence| &evidence.check.name == name))
            });
            (selected.is_empty() || missing)
                && !selected
                    .iter()
                    .any(|evidence| evidence.check.bucket == "skipping")
        })
    {
        result.state = CiState::Pending;
        result.summary.push_str(" awaiting check registration");
    }
    result.head_sha = Some(before.sha.clone());
    if let Some(evidence) = evidence {
        result.failed_checks = evidence
            .into_iter()
            .filter(|evidence| {
                matches!(evidence.check.bucket.as_str(), "fail" | "cancel")
                    && required
                        .as_ref()
                        .is_none_or(|(_, names)| names.contains(&evidence.check.name))
            })
            .map(|evidence| CiFailedCheck {
                extra: Default::default(),
                name: evidence.check.name,
                url: evidence.url,
                excerpt: evidence.excerpt,
            })
            .collect();
    }
    // Checks and the mutable branch are separate API reads. Never let a green
    // result for the previous push settle the gate after the branch has moved.
    match resolve_head(target, read) {
        Some(after) if after == before => result,
        Some(after) => CiObservation {
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

fn read_evidence(head: &Head, read: &dyn Fn(&[&str]) -> Option<Vec<u8>>) -> Option<Vec<Evidence>> {
    let root = format!("repos/{}/commits/{}", head.repository, head.sha);
    let check_endpoint = format!("{root}/check-runs?per_page=100&filter=latest");
    let checks: Vec<serde_json::Value> = serde_json::from_slice(&read(&[
        "gh",
        "api",
        "--paginate",
        "--slurp",
        &check_endpoint,
    ])?)
    .ok()?;
    let status_endpoint = format!("{root}/statuses?per_page=100");
    let statuses: Vec<Vec<serde_json::Value>> = serde_json::from_slice(&read(&[
        "gh",
        "api",
        "--paginate",
        "--slurp",
        &status_endpoint,
    ])?)
    .ok()?;
    parse_evidence(head, &checks, &statuses)
}

fn parse_evidence(
    head: &Head,
    pages: &[serde_json::Value],
    status_pages: &[Vec<serde_json::Value>],
) -> Option<Vec<Evidence>> {
    let mut result = Vec::new();
    for page in pages {
        for value in page.get("check_runs")?.as_array()? {
            if value.get("head_sha")?.as_str()? != head.sha {
                return None;
            }
            let status = value.get("status")?.as_str()?;
            let bucket = match status {
                "completed" => match value.get("conclusion")?.as_str()? {
                    "success" => "pass",
                    "skipped" | "neutral" => "skipping",
                    "cancelled" => "cancel",
                    "failure" | "timed_out" | "action_required" | "startup_failure" | "stale" => {
                        "fail"
                    }
                    _ => return None,
                },
                "queued" | "in_progress" | "pending" | "waiting" | "requested" => "pending",
                _ => return None,
            };
            let output = value.get("output");
            let excerpt = ["title", "summary", "text"]
                .into_iter()
                .filter_map(|key| output?.get(key)?.as_str())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            result.push(Evidence {
                check: Check {
                    name: value.get("name")?.as_str()?.into(),
                    bucket: bucket.into(),
                },
                url: value
                    .get("details_url")
                    .and_then(|url| url.as_str())
                    .map(str::to_owned),
                excerpt,
            });
        }
    }
    // The statuses endpoint is newest first, including prior updates to the
    // same context. Only its latest state belongs in the observation.
    let mut seen = std::collections::BTreeSet::new();
    for value in status_pages.iter().flatten() {
        let name = value.get("context")?.as_str()?;
        if !seen.insert(name) {
            continue;
        }
        if value
            .get("sha")
            .and_then(|v| v.as_str())
            .is_some_and(|sha| sha != head.sha)
        {
            return None;
        }
        let bucket = match value.get("state")?.as_str()? {
            "success" => "pass",
            "pending" | "expected" => "pending",
            "error" | "failure" => "fail",
            _ => return None,
        };
        result.push(Evidence {
            check: Check {
                name: name.into(),
                bucket: bucket.into(),
            },
            url: value
                .get("target_url")
                .and_then(|url| url.as_str())
                .map(str::to_owned),
            excerpt: value
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .into(),
        });
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::{Cell, RefCell};

    const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NEW: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn pull(sha: &str) -> serde_json::Value {
        json!({"headRefOid":sha,"headRepository":{"name":"repo"},"headRepositoryOwner":{"login":"owner"}})
    }

    fn checks(sha: &str, status: &str, conclusion: Option<&str>) -> serde_json::Value {
        json!([{"check_runs":[{
            "name":"build","head_sha":sha,"status":status,"conclusion":conclusion,
            "details_url":"https://github.com/owner/repo/actions/runs/1",
            "output":{"title":"Build failed","summary":"error: expected a value","text":"src/main.rs:12"}
        }]}])
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
        let observation = observe_with("feature", &["build".into()], &read, |_| None);
        assert_eq!(observation.state, CiState::Failed);
        assert_eq!(observation.head_sha.as_deref(), Some(OLD));
        assert_eq!(observation.check_names, ["build"]);
        assert_eq!(observation.failed_checks.len(), 1);
        assert_eq!(observation.failed_checks[0].name, "build");
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
        let observation = observe_with("feature", &["build".into()], &read, |_| None);
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
            let observation = observe_with("feature", &["build".into()], &read, |_| None);
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
        let observation = observe_with("feature/repair", &["build".into()], &read, |_| None);
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
        let observation = observe_with("refs/heads/123", &["build".into()], &read, |_| None);
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
    fn ci_probe_uses_the_latest_legacy_status_per_context() {
        let head = Head {
            repository: "owner/repo".into(),
            sha: OLD.into(),
        };
        let evidence = parse_evidence(
            &head,
            &[json!({"check_runs":[]})],
            &[vec![
                json!({"context":"legacy","state":"success"}),
                json!({"context":"legacy","state":"failure","description":"obsolete failure"}),
            ]],
        )
        .unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].check.bucket, "pass");
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
            let observation =
                observe_with("feature", &["build".into(), "lint".into()], &read, |_| None);
            assert_eq!(observation.state, expected, "{check_pages}");
            if expected == CiState::Pending {
                assert!(observation.summary.contains("awaiting check registration"));
            }
        }
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
