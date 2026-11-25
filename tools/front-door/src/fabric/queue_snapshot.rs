//! Resolve delivery selectors to bounded, immutable tracker snapshots.
use provenant_config::{Forge, ForgeOperation, ForgePort, PortBackend, PortTable};
use super::queue_registry::{QueueIdentity, QueueSnapshot, QueueSnapshotItem};
use provenant_core::{CancelToken, Clock};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, time::Duration};

const MAX_ITEMS: usize = 256;
const MAX_QUERY_BYTES: usize = 1024;
const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
const MAX_READ_BUDGET: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Selector {
    Issues(Vec<u64>),
    Query(String),
}

fn parse_selector(values: &[String]) -> Result<(Selector, String), String> {
    let Some(first) = values.first() else {
        return Err("delivery queue requires --over issues:N,M or --over query:SEARCH".into());
    };
    if let Some(first_numbers) = first.strip_prefix("issues:") {
        let mut numbers = Vec::new();
        for value in std::iter::once(first_numbers).chain(values.iter().skip(1).map(String::as_str)) {
            if value.is_empty() {
                continue;
            }
            for number in value.split(',') {
                let parsed = number
                    .parse::<u64>()
                    .ok()
                    .filter(|number| *number > 0)
                    .ok_or_else(|| format!("invalid delivery issue number `{number}`"))?;
                if !numbers.contains(&parsed) {
                    numbers.push(parsed);
                }
            }
        }
        if numbers.is_empty() || numbers.len() > MAX_ITEMS {
            return Err(format!("delivery issue selection requires 1..={MAX_ITEMS} items"));
        }
        let selector = format!("issues:{}", numbers.iter().map(u64::to_string).collect::<Vec<_>>().join(","));
        return Ok((Selector::Issues(numbers), selector));
    }
    if let Some(query) = first.strip_prefix("query:") {
        if values.len() != 1 {
            return Err("tracker query must be one --over value; quote it to preserve spaces".into());
        }
        let query = query.trim();
        if query.is_empty() || query.len() > MAX_QUERY_BYTES || query.contains('\0') {
            return Err(format!("tracker query must contain 1..={MAX_QUERY_BYTES} bytes"));
        }
        return Ok((Selector::Query(query.to_owned()), format!("query:{query}")));
    }
    Err("delivery selector must start with `issues:` or `query:`".into())
}

/// Resolve a delivery selector using the repository's tracker port. Search
/// queries are scoped to the selected repository, capped to one complete page,
/// and rejected when the tracker reports another page.
pub(super) fn resolve_snapshot(
    project: &Path,
    state: &Path,
    project_id: &str,
    values: &[String],
    cancel: &CancelToken,
    budget: Duration,
) -> Result<QueueSnapshot, String> {
    let (selector, selector_text) = parse_selector(values)?;
    let ports = PortTable::read_for(Some(project), state)
        .map_err(|error| format!("tracker port configuration unavailable: {error}"))?;
    if matches!(ports.tracker.backend, PortBackend::None) {
        return Err("delivery queue tracker port is disabled".into());
    }
    let policy = provenant_config::operation_policy(state, project, &ports);
    let mut forge = ForgePort::new(ports.tracker, "tracker", project, state, policy)
        .map_err(|error| format!("cannot start tracker adapter: {error}"))?;
    let request = request(cancel, budget);
    forge.bind(&request);
    let repository_response = forge
        .request(ForgeOperation::Repository, request.remaining())
        .map_err(|error| format!("cannot resolve tracker repository: {error}"))?;
    let repository_data = response_json(repository_response.status, &repository_response.body)?;
    let repository = repository_data["nameWithOwner"]
        .as_str()
        .filter(|value| valid_repository(value))
        .ok_or("tracker repository response has no valid nameWithOwner")?
        .to_owned();
    let raw = match &selector {
        Selector::Issues(numbers) => {
            let (owner, name) = repository
                .split_once('/')
                .ok_or("tracker repository identity is malformed")?;
            let fields = numbers
                .iter()
                .map(|number| {
                    format!(
                        "issue_{number}: issue(number: {number}) {{ id number title body state url repository {{ nameWithOwner }} }}"
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            let query = format!(
                "query {{ repository(owner: {}, name: {}) {{ {fields} }} }}",
                graphql_string(owner)?,
                graphql_string(name)?
            );
            let response = forge
                .request(ForgeOperation::Graphql { query }, request.remaining())
                .map_err(|error| format!("cannot resolve tracker issues: {error}"))?;
            let data = response_json(response.status, &response.body)?;
            let repository_object = data
                .get("repository")
                .and_then(Value::as_object)
                .ok_or("tracker issue response has no repository")?;
            numbers
                .iter()
                .map(|number| {
                    let issue = repository_object
                        .get(&format!("issue_{number}"))
                        .filter(|issue| !issue.is_null())
                        .ok_or_else(|| format!("tracker issue #{number} was not found"))?;
                    checked_issue(issue, &repository, Some(*number))
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        Selector::Query(query_text) => {
            let query = format!(
                "query {{ search(type: ISSUE, first: {MAX_ITEMS}, query: {}) {{ issueCount pageInfo {{ hasNextPage }} nodes {{ __typename ... on Issue {{ id number title body state url repository {{ nameWithOwner }} }} }} }} }}",
                graphql_string(&format!("repo:{repository} is:issue {query_text}"))?
            );
            let response = forge
                .request(ForgeOperation::Graphql { query }, request.remaining())
                .map_err(|error| format!("cannot resolve tracker query snapshot: {error}"))?;
            let data = response_json(response.status, &response.body)?;
            let search = data
                .get("search")
                .and_then(Value::as_object)
                .ok_or("tracker query response has no search result")?;
            if search["pageInfo"]["hasNextPage"] != Value::Bool(false) {
                return Err(format!(
                    "tracker query exceeds one complete page of {MAX_ITEMS} issues; narrow the query"
                ));
            }
            let nodes = search["nodes"]
                .as_array()
                .ok_or("tracker query response has no issue list")?;
            if nodes.len() > MAX_ITEMS {
                return Err(format!("tracker query returned more than {MAX_ITEMS} issues"));
            }
            let mut issues = Vec::with_capacity(nodes.len());
            for node in nodes {
                if node["__typename"] != "Issue" {
                    continue;
                }
                issues.push(checked_issue(node, &repository, None)?);
            }
            if search["issueCount"].as_u64().is_none() {
                return Err("tracker query response has no issue count".into());
            }
            issues
        }
    };
    if raw.is_empty() {
        return Err("tracker selector resolved to no issues".into());
    }
    let clock = provenant_os::clock::SystemClock::new();
    let resolved_at = clock
        .wall_now()
        .map_err(|error| format!("cannot timestamp tracker snapshot: {error}"))?;
    let tracker = format!("github:{repository}");
    let mut seen = BTreeSet::new();
    let items = raw
        .into_iter()
        .map(|issue| {
            let number = issue["number"]
                .as_u64()
                .ok_or("tracker issue has no number")?;
            let node_id = issue["id"].as_str().ok_or("tracker issue has no global id")?;
            if !seen.insert(node_id.to_owned()) {
                return Err(format!("tracker selector returned issue #{number} more than once"));
            }
            Ok(QueueSnapshotItem {
                identity: QueueIdentity::new(project_id, &tracker, node_id)?,
                data: issue,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(QueueSnapshot {
        selector: selector_text,
        source: match selector {
            Selector::Issues(_) => "explicit_issues",
            Selector::Query(_) => "tracker_query",
        }
        .into(),
        resolved_at,
        items,
    })
}

#[allow(clippy::disallowed_methods, reason = "tracker request owns its absolute deadline")]
fn request(cancel: &CancelToken, budget: Duration) -> provenant_config::ports::Request {
    let budget = budget.min(MAX_READ_BUDGET);
    let deadline = std::time::Instant::now()
        .checked_add(budget)
        .unwrap_or_else(|| std::time::Instant::now() + MAX_READ_BUDGET);
    provenant_config::ports::Request::new(deadline, cancel.clone())
}

fn checked_issue(issue: &Value, repository: &str, expected_number: Option<u64>) -> Result<Value, String> {
    let issue_repository = issue["repository"]["nameWithOwner"]
        .as_str()
        .ok_or("tracker issue has no repository identity")?;
    let number = issue["number"]
        .as_u64()
        .ok_or("tracker issue has no number")?;
    if issue_repository != repository || expected_number.is_some_and(|expected| expected != number) {
        return Err("tracker issue identity does not match the selected repository and number".into());
    }
    for field in ["id", "title", "url", "state"] {
        if issue[field].as_str().is_none() {
            return Err(format!("tracker issue #{number} has no {field}"));
        }
    }
    let body = issue["body"].as_str().unwrap_or_default();
    if body.len() > MAX_RESULT_BYTES {
        return Err(format!("tracker issue #{number} body exceeds the snapshot limit"));
    }
    Ok(json!({
        "id": issue["id"],
        "number": number,
        "title": issue["title"],
        "body": body,
        "state": issue["state"],
        "url": issue["url"],
        "repository": repository,
    }))
}

fn response_json(status: Option<u16>, bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_RESULT_BYTES {
        return Err("tracker response exceeds the snapshot limit".into());
    }
    if status.is_some_and(|status| !(200..300).contains(&status)) {
        return Err(format!("tracker returned HTTP status {}", status.unwrap_or_default()));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid tracker response: {error}"))?;
    if value.get("errors").and_then(Value::as_array).is_some_and(|errors| !errors.is_empty()) {
        return Err("tracker query returned GraphQL errors".into());
    }
    value
        .get("data")
        .filter(|data| !data.is_null())
        .cloned()
        .or_else(|| value.get("nameWithOwner").map(|_| value.clone()))
        .ok_or_else(|| "tracker response has no data".into())
}

fn graphql_string(value: &str) -> Result<String, String> {
    serde_json::to_string(value).map_err(|_| "cannot encode tracker query".into())
}

fn valid_repository(value: &str) -> bool {
    let Some((owner, name)) = value.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && !name.is_empty()
        && !name.contains('/')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')
        })
}
