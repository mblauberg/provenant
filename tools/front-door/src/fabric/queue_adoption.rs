//! Resolve an explicitly adopted pull request to a sealed queue intent.
//!
//! This module only observes remote and local state. Graph execution rechecks
//! the intent under its journal lock before it creates the typed publication
//! receipt or advances the conflict gate.

use super::{Factory, queue_registry::QueueSnapshotItem};
use provenant_config::{ForgeOperation, ForgePort};
use provenant_core::{CancelToken, WaitBudget};
use provenant_confinement::{repo::GitRefName, worktrees};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, time::Duration};

const MAX_READ_BUDGET: Duration = Duration::from_secs(60);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_ISSUE_REFERENCES: usize = 100;

/// Host observations sealed with one queue item. This is an untrusted graph
/// intent: execution must independently revalidate every field before using it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AdoptedPullRequest {
    pub schema_version: u64,
    pub operation_id: String,
    pub project: String,
    pub tracker: String,
    pub item: String,
    pub repository: String,
    pub issue_identity: String,
    pub issue_number: u64,
    pub pull_request: u64,
    pub pull_request_url: String,
    pub branch: String,
    pub worktree: String,
    pub source_directory_identity: provenant_records::run::SourceIdentity,
    pub head: String,
    pub base_repository: String,
    pub base_branch: String,
    pub base_head: String,
    pub custody_host: String,
    pub custody_attempt: String,
    pub custody_operation: String,
}

struct WorktreeCustody {
    path: String,
    identity: provenant_records::run::SourceIdentity,
    host: String,
    attempt: String,
    operation: String,
}

#[derive(Serialize)]
struct AdoptionIdentity<'a> {
    item: &'a super::queue_registry::QueueIdentity,
    repository: &'a str,
    pull_request: u64,
    pull_request_url: &'a str,
    issue_identity: &'a str,
    issue_number: u64,
    branch: &'a str,
    worktree: &'a str,
    source_directory_identity: &'a provenant_records::run::SourceIdentity,
    head: &'a str,
    base_repository: &'a str,
    base_branch: &'a str,
    base_head: &'a str,
    custody_host: &'a str,
    custody_attempt: &'a str,
    custody_operation: &'a str,
}

/// Resolve and validate an open PR for one already-snapshotted issue.
pub(super) fn resolve(
    factory: &Factory,
    item: &QueueSnapshotItem,
    pull_request: u64,
    cancel: &CancelToken,
    budget: Duration,
) -> Result<AdoptedPullRequest, String> {
    if pull_request == 0 {
        return Err("adopted pull request number must be positive".into());
    }
    let repository = snapshot_repository(item)?;
    let issue_number = item.data["number"]
        .as_u64()
        .filter(|number| *number > 0)
        .ok_or("queue snapshot issue has no valid number")?;
    let issue_identity = item.data["id"]
        .as_str()
        .filter(|identity| !identity.trim().is_empty() && identity.len() <= 512)
        .ok_or("queue snapshot issue has no valid global identity")?;
    if issue_identity != item.identity.item {
        return Err("queue item identity does not match its tracker snapshot".into());
    }
    let project = factory.context.identity.project.to_string();
    if !item.identity.project.eq_ignore_ascii_case(&project) {
        return Err("queue item project does not match the selected project".into());
    }

    let pull = resolve_pull_request(
        factory,
        &repository,
        pull_request,
        issue_identity,
        issue_number,
        cancel,
        budget,
    )?;
    let branch_ref = GitRefName::branch(&pull.branch)
        .map_err(|_| "adopted pull request has an invalid branch name")?;
    let custody = resolve_worktree(factory, &branch_ref, &pull.head, cancel)?;
    let operation_id = adoption_operation_id(&AdoptionIdentity {
        item: &item.identity,
        repository: &repository,
        pull_request,
        pull_request_url: &pull.url,
        issue_identity,
        issue_number,
        branch: &pull.branch,
        worktree: &custody.path,
        source_directory_identity: &custody.identity,
        head: &pull.head,
        base_repository: &pull.base_repository,
        base_branch: &pull.base_branch,
        base_head: &pull.base_head,
        custody_host: &custody.host,
        custody_attempt: &custody.attempt,
        custody_operation: &custody.operation,
    })?;

    Ok(AdoptedPullRequest {
        schema_version: 1,
        operation_id,
        project,
        tracker: item.identity.tracker.clone(),
        item: item.identity.item.clone(),
        repository,
        issue_identity: issue_identity.to_owned(),
        issue_number,
        pull_request,
        pull_request_url: pull.url,
        branch: pull.branch,
        worktree: custody.path,
        source_directory_identity: custody.identity,
        head: pull.head,
        base_repository: pull.base_repository,
        base_branch: pull.base_branch,
        base_head: pull.base_head,
        custody_host: custody.host,
        custody_attempt: custody.attempt,
        custody_operation: custody.operation,
    })
}

/// Recheck a persisted queue adoption intent and return its host-authored
/// publication receipt. The caller holds the graph journal lock while this
/// function runs; the queue registry lock authenticates the item and operation.
pub(super) fn revalidate(
    factory: &Factory,
    request: &provenant_fabric_runtime::graph::DeliveryAdoptionRequest<'_>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<provenant_fabric_runtime::graph::DeliveryPublication, String> {
    const SEED_NODE: &str = "adopted-publish";
    let intent = request.intent;
    if request.node_id != SEED_NODE || !valid_intent(intent) {
        return Err("adoption seed request is invalid".into());
    }
    let identity = super::queue_registry::QueueIdentity::new(
        &intent.project,
        &intent.tracker,
        &intent.item,
    )?;
    if identity.project != intent.project || identity.tracker != intent.tracker {
        return Err("adoption tracker identity is not canonical".into());
    }
    let registry = super::queue_registry::QueueRegistry::open(
        factory.context.roots.state.as_path(),
    )?;
    let persisted = registry.adoption_for(
        request.graph_id,
        &identity,
        &intent.operation_id,
        budget,
        cancel,
        &factory.clock,
    )?;
    if !intent_matches(&persisted, intent) {
        return Err("adoption request differs from its authenticated queue intent".into());
    }

    let pull = resolve_pull_request(
        factory,
        &intent.repository,
        intent.pull_request,
        &intent.issue_identity,
        intent.issue_number,
        cancel,
        Duration::from_micros(
            budget
                .remaining(factory.clock.mono_now())
                .as_micros(),
        ),
    )?;
    if pull.url != intent.pull_request_url
        || pull.branch != intent.branch
        || pull.head != intent.head
        || !same_repository(&pull.base_repository, &intent.base_repository)
        || pull.base_branch != intent.base_branch
        || pull.base_head != intent.base_head
    {
        return Err("live pull request differs from the sealed adoption intent".into());
    }

    let branch = GitRefName::branch(&intent.branch)
        .map_err(|_| "adoption intent has an invalid branch")?;
    let custody = resolve_worktree(factory, &branch, &intent.head, cancel)?;
    if custody.path != intent.worktree
        || custody.identity != intent.source_directory_identity
        || custody.host != intent.custody_host
        || custody.attempt != intent.custody_attempt
        || custody.operation != intent.custody_operation
    {
        return Err("live worktree custody differs from the sealed adoption intent".into());
    }
    Ok(provenant_fabric_runtime::graph::DeliveryPublication {
        branch: intent.branch.clone(),
        head: intent.head.clone(),
        pull_request: intent.pull_request_url.clone(),
    })
}

fn intent_matches(
    persisted: &AdoptedPullRequest,
    intent: &provenant_fabric_runtime::graph::DeliveryAdoptionIntent,
) -> bool {
    persisted.schema_version == 1
        && persisted.operation_id == intent.operation_id
        && persisted.project == intent.project
        && persisted.tracker == intent.tracker
        && persisted.item == intent.item
        && same_repository(&persisted.repository, &intent.repository)
        && persisted.issue_identity == intent.issue_identity
        && persisted.issue_number == intent.issue_number
        && persisted.pull_request == intent.pull_request
        && persisted.pull_request_url == intent.pull_request_url
        && persisted.branch == intent.branch
        && persisted.worktree == intent.worktree
        && persisted.source_directory_identity == intent.source_directory_identity
        && persisted.custody_host == intent.custody_host
        && persisted.custody_attempt == intent.custody_attempt
        && persisted.custody_operation == intent.custody_operation
        && persisted.head == intent.head
        && same_repository(&persisted.base_repository, &intent.base_repository)
        && persisted.base_branch == intent.base_branch
        && persisted.base_head == intent.base_head
}

fn valid_intent(intent: &provenant_fabric_runtime::graph::DeliveryAdoptionIntent) -> bool {
    let is_digest = intent.operation_id.strip_prefix("sha256:").is_some_and(|value| {
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    let nonempty = |value: &str| {
        !value.trim().is_empty() && value.trim() == value && !value.chars().any(char::is_control)
    };
    is_digest
        && [
            intent.project.as_str(),
            intent.tracker.as_str(),
            intent.item.as_str(),
            intent.repository.as_str(),
            intent.issue_identity.as_str(),
            intent.pull_request_url.as_str(),
            intent.branch.as_str(),
            intent.worktree.as_str(),
            intent.custody_host.as_str(),
            intent.custody_attempt.as_str(),
            intent.custody_operation.as_str(),
            intent.base_repository.as_str(),
            intent.base_branch.as_str(),
        ]
        .into_iter()
        .all(nonempty)
        && intent.pull_request > 0
        && intent.issue_number > 0
        && intent.issue_identity == intent.item
        && intent
            .tracker
            .eq_ignore_ascii_case(&format!("github:{}", intent.repository))
        && valid_repository(&intent.repository)
        && same_repository(&intent.repository, &intent.base_repository)
        && intent.pull_request_url.starts_with("https://")
        && intent.worktree.starts_with('/')
        && GitRefName::branch(&intent.branch).is_ok()
        && intent.custody_host.parse::<provenant_core::HostId>().is_ok()
        && intent
            .custody_attempt
            .parse::<provenant_core::AttemptId>()
            .is_ok()
        && intent
            .custody_operation
            .parse::<provenant_core::OperationId>()
            .is_ok()
        && intent.head.parse::<provenant_core::GitOid>().is_ok()
        && intent.base_head.parse::<provenant_core::GitOid>().is_ok()
}

struct PullFacts {
    url: String,
    branch: String,
    head: String,
    base_repository: String,
    base_branch: String,
    base_head: String,
}

fn resolve_pull_request(
    factory: &Factory,
    repository: &str,
    number: u64,
    issue_identity: &str,
    issue_number: u64,
    cancel: &CancelToken,
    budget: Duration,
) -> Result<PullFacts, String> {
    let (owner, name) = repository
        .split_once('/')
        .ok_or("tracker repository identity is malformed")?;
    let repository_field = graphql_string(owner)?;
    let name_field = graphql_string(name)?;
    let query = format!(
        "query {{ repository(owner: {repository_field}, name: {name_field}) {{ pullRequest(number: {number}) {{ number url state headRefName headRefOid baseRefName baseRefOid headRepository {{ nameWithOwner }} baseRepository {{ nameWithOwner }} closingIssuesReferences(first: {MAX_ISSUE_REFERENCES}) {{ nodes {{ id number repository {{ nameWithOwner }} }} pageInfo {{ hasNextPage }} }} }} }} }}"
    );
    let ports = &factory.context.port_table;
    if matches!(
        &ports.forge.backend,
        &provenant_config::PortBackend::None
    ) {
        return Err("delivery queue forge port is disabled".into());
    }
    let project = factory.context.project_root.as_path();
    let state = factory.context.roots.state.as_path();
    let policy = provenant_config::operation_policy(state, project, ports);
    let mut forge = ForgePort::new(ports.forge.clone(), "forge", project, state, policy)
        .map_err(|error| format!("cannot start forge adapter: {error}"))?;
    let request = request(cancel, budget);
    let local_repository = crate::pr::observer::repository_for_project(state, project, &request)
        .map_err(|error| format!("cannot resolve selected project repository: {error}"))?;
    if !same_repository(&local_repository, repository) {
        return Err("tracker and pull-request repository do not match the selected project".into());
    }
    forge.bind(&request);
    let response = forge
        .request(ForgeOperation::Graphql { query }, request.remaining())
        .map_err(|error| format!("cannot resolve adopted pull request: {error}"))?;
    let data = response_json(response.status, &response.body)?;
    let pull = data
        .pointer("/repository/pullRequest")
        .filter(|value| !value.is_null())
        .ok_or_else(|| format!("pull request #{number} was not found in {repository}"))?;
    if pull["number"].as_u64() != Some(number) || pull["state"].as_str() != Some("OPEN") {
        return Err(format!("pull request #{number} is not open in {repository}"));
    }
    let head_repository = repository_field_value(&pull["headRepository"], "head")?;
    let base_repository = repository_field_value(&pull["baseRepository"], "base")?;
    if !same_repository(&head_repository, repository)
        || !same_repository(&base_repository, repository)
    {
        return Err("adopted pull request must use the selected repository for both head and base".into());
    }
    let references = pull
        .pointer("/closingIssuesReferences/nodes")
        .and_then(Value::as_array)
        .ok_or("pull request response has no closing issue references")?;
    if references.len() > MAX_ISSUE_REFERENCES {
        return Err("pull request returned more closing issue references than requested".into());
    }
    if pull.pointer("/closingIssuesReferences/pageInfo/hasNextPage") != Some(&Value::Bool(false)) {
        return Err("pull request closing issue references are incomplete".into());
    }
    let matches = references
        .iter()
        .filter(|reference| {
            reference["id"].as_str() == Some(issue_identity)
                && reference["number"].as_u64() == Some(issue_number)
                && reference
                    .pointer("/repository/nameWithOwner")
                    .and_then(Value::as_str)
                    .is_some_and(|reference_repository| {
                        same_repository(reference_repository, repository)
                    })
        })
        .count();
    if matches != 1 {
        return Err("pull request does not close exactly the selected tracker issue identity".into());
    }

    let url = pull["url"]
        .as_str()
        .filter(|url| {
            url.len() <= 2048
                && url.starts_with("https://")
                && !url.chars().any(char::is_control)
        })
        .ok_or("pull request response has no valid URL")?
        .to_owned();
    let branch = pull["headRefName"]
        .as_str()
        .filter(|branch| !branch.is_empty() && branch.len() <= 256)
        .ok_or("pull request response has no valid head branch")?
        .to_owned();
    let head = parse_oid(
        pull["headRefOid"]
            .as_str()
            .ok_or("pull request response has no valid head commit")?,
        "head",
    )?;
    let base_branch = pull["baseRefName"]
        .as_str()
        .filter(|branch| !branch.is_empty() && branch.len() <= 256)
        .ok_or("pull request response has no valid base branch")?
        .to_owned();
    GitRefName::branch(&base_branch)
        .map_err(|_| "pull request response has an invalid base branch")?;
    let base_head = parse_oid(
        pull["baseRefOid"]
            .as_str()
            .ok_or("pull request response has no valid base commit")?,
        "base",
    )?;
    Ok(PullFacts {
        url,
        branch,
        head,
        base_repository,
        base_branch,
        base_head,
    })
}

fn resolve_worktree(
    factory: &Factory,
    branch: &GitRefName,
    expected_head: &str,
    cancel: &CancelToken,
) -> Result<WorktreeCustody, String> {
    let entries = worktrees::with_context_using_workers(
        &factory.context.roots,
        &factory.home,
        &factory.context.project_root,
        &factory.clock,
        cancel,
        factory.workers,
        |context| {
            let matches = worktrees::list_for_gc_matching(context, cancel, |_, candidate| {
                candidate == Some(branch.as_str())
            })?;
            if matches.len() != 1 {
                return Err(worktrees::WorktreeError::InvalidRequest);
            }
            let info = &matches[0];
            if info.primary
                || info.branch.as_deref() != Some(branch.as_str())
                || info.head.to_string() != expected_head
                || info.dirty != worktrees::WorktreeCondition::Clean
                || !info.occupancy.is_available()
            {
                return Err(worktrees::WorktreeError::InvalidRequest);
            }
            let owner = info
                .owner
                .as_ref()
                .map_err(|_| worktrees::WorktreeError::InvalidRequest)?
                .as_ref()
                .ok_or(worktrees::WorktreeError::InvalidRequest)?;
            if owner.branch != branch.short_name()
                || owner.host != factory.context.identity.host
            {
                return Err(worktrees::WorktreeError::InvalidRequest);
            }
            let attempt = owner
                .attempt_id
                .ok_or(worktrees::WorktreeError::InvalidRequest)?;
            let owner_operation = owner
                .operation_id
                .clone()
                .ok_or(worktrees::WorktreeError::InvalidRequest)?;

            let mut tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
            let identity = tree.identity();
            let _guard = worktrees::cache_writer_guard(context.project_state, tree.path())
                .map_err(worktrees::custody_refusal)?;
            worktrees::require_clean_worktree(context, &mut tree, cancel)?;
            let git = provenant_os::git::git_executable()
                .ok_or(worktrees::WorktreeError::InvalidRequest)?;
            let live_head = provenant_run::resolve_git_base_with_facts(
                &git,
                tree.path(),
                "HEAD",
                context.facts,
                cancel,
            )
            .map_err(|_| worktrees::WorktreeError::InvalidRequest)?;
            if live_head != expected_head {
                return Err(worktrees::WorktreeError::InvalidRequest);
            }
            validate_owner_attempt(
                factory,
                attempt,
                &owner_operation,
                tree.path(),
                expected_head,
                cancel,
            )
                .map_err(|_| worktrees::WorktreeError::InvalidRequest)?;
            let path = tree
                .path()
                .to_str()
                .filter(|path| !path.is_empty() && Path::new(path).is_absolute())
                .ok_or(worktrees::WorktreeError::InvalidRequest)?
                .to_owned();
            Ok(WorktreeCustody {
                path,
                identity: provenant_records::run::SourceIdentity {
                    device: identity.device,
                    inode: identity.inode,
                },
                host: owner.host.to_string(),
                attempt: attempt.to_string(),
                operation: owner_operation.to_string(),
            })
        },
    )
    .map_err(|error| format!("cannot validate adopted worktree custody: {error}"))?;
    Ok(entries)
}

fn validate_owner_attempt(
    factory: &Factory,
    attempt: provenant_core::AttemptId,
    owner_operation: &OperationId,
    worktree: &Path,
    expected_head: &str,
    cancel: &CancelToken,
) -> Result<(), String> {
    let execution = provenant_run::ExecutionContext {
        state_root: factory.context.roots.state.clone(),
        project_id: factory.context.identity.project,
        actor_id: provenant_core::ActorId::from_bytes([0; 16]),
        read_prompts: false,
    };
    let call = provenant_run::Request {
        clock: &factory.clock,
        cancel,
    };
    let store = provenant_run::RunStore::open_disk_read_only(&execution, &call)
        .map_err(|error| format!("cannot read adopted worktree owner attempt: {error}"))?
        .ok_or("adopted worktree owner attempt store is unavailable")?;
    let record = store
        .record(attempt, &call)
        .map_err(|error| format!("cannot verify adopted worktree owner attempt: {error}"))?;
    if record.attempt_id != attempt
        || record.project_id != factory.context.identity.project
        || !matches!(
            record.state,
            provenant_records::run::AttemptState::Terminal { .. }
        )
        || record.head.map(|head| head.to_string()).as_deref() != Some(expected_head)
        || !store
            .custody_released(&record, &call)
            .map_err(|error| format!("cannot verify adopted worktree custody release: {error}"))?
    {
        return Err("adopted worktree owner attempt is not terminal with released custody at the PR head".into());
    }
    let operation = store
        .attempt_operation(attempt, &call)
        .map_err(|error| format!("cannot verify adopted worktree owner operation: {error}"))?;
    if &operation.operation_id != owner_operation {
        return Err("adopted worktree owner operation does not match its attempt".into());
    }
    let writer_request = provenant_run::resolve_writer(
        factory.context.roots.state.as_path(),
        &factory.context.project_root,
        worktree,
    )
    .map_err(|error| format!("cannot verify adopted worktree launch binding: {error}"))?;
    let previous_task = store
        .request(attempt, &call)
        .map_err(|error| format!("cannot verify adopted worktree launch: {error}"))?
        .task;
    if !previous_task
        .writer
        .as_ref()
        .is_some_and(|previous| provenant_run::equivalent_writer(previous, &writer_request))
    {
        return Err("adopted worktree owner attempt does not bind this exact worktree".into());
    }
    Ok(())
}

fn snapshot_repository(item: &QueueSnapshotItem) -> Result<String, String> {
    let repository = item.data["repository"]
        .as_str()
        .filter(|repository| valid_repository(repository))
        .ok_or("queue tracker snapshot has no valid repository")?;
    let tracker_repository = item
        .identity
        .tracker
        .strip_prefix("github:")
        .ok_or("queue item tracker is not GitHub")?;
    if !same_repository(repository, tracker_repository) {
        return Err("queue tracker repository does not match the snapshot repository".into());
    }
    Ok(repository.to_owned())
}

fn repository_field_value(value: &Value, label: &str) -> Result<String, String> {
    let repository = value
        .get("nameWithOwner")
        .and_then(Value::as_str)
        .filter(|repository| valid_repository(repository))
        .ok_or_else(|| format!("pull request response has no valid {label} repository"))?;
    Ok(repository.to_owned())
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

fn same_repository(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn parse_oid(value: &str, label: &str) -> Result<String, String> {
    value
        .parse::<provenant_core::GitOid>()
        .map(|oid| oid.to_string())
        .map_err(|_| format!("pull request response has an invalid {label} commit"))
}

fn response_json(status: Option<u16>, bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err("forge response exceeds the adoption limit".into());
    }
    if status.is_some_and(|status| !(200..300).contains(&status)) {
        return Err(format!("forge returned HTTP status {}", status.unwrap_or_default()));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid forge response: {error}"))?;
    if value
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err("forge query returned GraphQL errors".into());
    }
    value
        .get("data")
        .filter(|data| !data.is_null())
        .cloned()
        .ok_or_else(|| "forge response has no data".into())
}

fn graphql_string(value: &str) -> Result<String, String> {
    serde_json::to_string(value).map_err(|_| "cannot encode forge query".into())
}

fn adoption_operation_id(identity: &AdoptionIdentity<'_>) -> Result<String, String> {
    let digest = provenant_records::CanonicalV1::record_digest(
        "delivery-queue-adoption",
        1,
        identity,
    )
    .map_err(|_| "cannot seal adopted pull request identity")?;
    Ok(digest.to_string())
}

#[allow(clippy::disallowed_methods, reason = "forge request uses one bounded absolute deadline")]
fn request(cancel: &CancelToken, budget: Duration) -> provenant_config::ports::Request {
    let budget = budget.min(MAX_READ_BUDGET);
    let now = std::time::Instant::now();
    let deadline = now.checked_add(budget).unwrap_or(now + MAX_READ_BUDGET);
    provenant_config::ports::Request::new(deadline, cancel.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mixed_case_snapshot_repository_matches_canonical_tracker_identity() {
        let item = QueueSnapshotItem {
            identity: super::super::queue_registry::QueueIdentity {
                project: "project".into(),
                tracker: "github:owner/repository".into(),
                item: "issue-node-id".into(),
            },
            data: json!({"repository":"Owner/Repository"}),
        };

        assert!(matches!(
            snapshot_repository(&item).as_deref(),
            Ok("Owner/Repository")
        ));
        assert!(!same_repository("Owner/Repository", "other/Repository"));
    }
}
