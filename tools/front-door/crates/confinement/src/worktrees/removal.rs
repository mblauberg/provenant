//! Removal of what a finished lane leaves behind: its worktree, local branch,
//! remote branch and remote-tracking ref. Every artefact reports one outcome.
//!
//! Only refs the ledger records as created by Provenant are candidates. A ref
//! it adopted, or never observed, is retained. Each deletion is leased on the
//! tip the caller just checked, so a ref that moved meanwhile is kept.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Artefact {
    Worktree,
    LocalBranch,
    RemoteBranch,
    TrackingRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtefactState {
    Removed,
    /// A preview found it removable.
    WouldRemove,
    /// It was already gone.
    Absent,
    Retained,
    /// Removal was attempted and did not complete; the artefact is retained.
    Failed,
}

/// One artefact's outcome. `name` is a worktree path or a full ref name;
/// `remote` names the bound remote for a remote branch or tracking ref.
/// `head` is the commit a removed ref held, which restores it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArtefactOutcome {
    pub artefact: Artefact,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    pub outcome: ArtefactState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<GitOid>,
}

impl ArtefactOutcome {
    pub fn new(artefact: Artefact, name: impl Into<String>, outcome: ArtefactState) -> Self {
        Self {
            artefact,
            name: name.into(),
            remote: None,
            outcome,
            reason: None,
            head: None,
        }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    pub fn with_head(mut self, head: GitOid) -> Self {
        self.head = Some(head);
        self
    }

    pub fn with_remote(mut self, remote: impl Into<String>) -> Self {
        self.remote = Some(remote.into());
        self
    }

    /// The outcomes a worktree removal reported, one per artefact.
    pub fn from_remove(outcome: &RemoveOutcome) -> Vec<Self> {
        let mut outcomes = Vec::new();
        for removed in &outcome.removed {
            outcomes.push(Self::new(
                Artefact::Worktree,
                removed.path.to_string_lossy(),
                ArtefactState::Removed,
            ));
        }
        for branch in &outcome.pruned_refs {
            outcomes.push(Self::new(
                Artefact::LocalBranch,
                format!("refs/heads/{branch}"),
                ArtefactState::Removed,
            ));
        }
        for discarded in &outcome.discarded {
            outcomes.push(
                Self::new(
                    Artefact::LocalBranch,
                    format!("refs/heads/{}", discarded.branch),
                    ArtefactState::Removed,
                )
                .with_head(discarded.head),
            );
        }
        for retained in &outcome.retained {
            let reason = serde_json::to_value(retained.reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "retained".into());
            let reason = match &retained.detail {
                Some(detail) => format!("{reason}: {detail}"),
                None => reason,
            };
            outcomes.push(
                Self::new(
                    Artefact::LocalBranch,
                    format!("refs/heads/{}", retained.branch),
                    ArtefactState::Retained,
                )
                .with_reason(reason),
            );
        }
        outcomes
    }
}

/// The ledger's current record for a local branch when Provenant created it
/// and, for a worktree owner that links a generation, still that generation.
/// Any other state, including an unreadable ledger, is `None`, which retains.
pub fn created_branch(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    owner: Option<&RefOwnership>,
) -> Option<RefRecord> {
    let record = RefLedger::for_context(context)
        .ok()?
        .load(&RefEndpoint::Local, branch)
        .ok()??;
    if record.origin != RefOrigin::Created {
        return None;
    }
    if let Some(owner) = owner
        && (owner.generation != record.generation
            || owner.origin != RefOrigin::Created
            || owner.repository != record.repository)
    {
        return None;
    }
    Some(record)
}

/// The shared landed check over the repository's object store, with any
/// receipt chain the caller holds.
pub fn landing_in_repo(
    context: &WorktreeContext<'_>,
    tip: GitOid,
    root: &LandingTarget,
    chain: &[IntegrationEdge],
    cancel: &CancelToken,
) -> Result<Landing, WorktreeError> {
    landed::landed(&RepoGraph { context, cancel }, tip, root, chain)
}

/// Whether the local object store holds `commit`, so a deleted ref that
/// pointed at it can be restored.
pub fn has_commit(
    context: &WorktreeContext<'_>,
    commit: GitOid,
    cancel: &CancelToken,
) -> Result<bool, WorktreeError> {
    Ok(RepoGraph { context, cancel }.parents(commit)?.is_some())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefDeletion {
    Removed,
    Absent,
    /// The ref no longer holds the expected tip.
    Changed,
    /// A worktree has the branch checked out.
    CheckedOut,
}

/// Delete a local branch or remote-tracking ref only while it holds
/// `expected`. A branch any worktree has checked out is kept. The repository
/// mutation lock serialises this with worktree creation and removal.
pub fn delete_ref_leased(
    context: &WorktreeContext<'_>,
    reference: &GitRefName,
    expected: GitOid,
    cancel: &CancelToken,
) -> Result<RefDeletion, WorktreeError> {
    let name = reference.as_str();
    if !(name.starts_with("refs/heads/") || name.starts_with("refs/remotes/")) {
        return Err(WorktreeError::InvalidRequest);
    }
    let _repository = repository_lock(context, LockMode::Exclusive, cancel)?;
    context.repo.revalidate()?;
    if name.starts_with("refs/heads/") {
        let fresh = context.facts.fresh_inventory(context, cancel)?;
        if checked_out_elsewhere(&fresh, reference, None)? {
            return Ok(RefDeletion::CheckedOut);
        }
    }
    match context
        .facts
        .resolve_commit(context, reference.clone(), cancel)
    {
        Ok(current) if current == expected => {}
        Ok(_) => return Ok(RefDeletion::Changed),
        Err(WorktreeError::NotFound) => return Ok(RefDeletion::Absent),
        Err(error) => return Err(error),
    }
    let _ = context.facts.take_inventory(context);
    let deleted = execute_git_mutation(
        context.executor,
        context.repo,
        context.utility,
        GitMutation::DeleteRef {
            name: reference.clone(),
            expected_old_oid: expected,
        },
        context.mutation_budget,
        cancel,
    )?;
    Ok(if exit_code(&deleted)? == 0 {
        RefDeletion::Removed
    } else {
        RefDeletion::Changed
    })
}

/// A branch's tip, or `None` when it does not exist.
pub fn ref_tip(
    context: &WorktreeContext<'_>,
    reference: &GitRefName,
    cancel: &CancelToken,
) -> Result<Option<GitOid>, WorktreeError> {
    match context
        .facts
        .resolve_commit(context, reference.clone(), cancel)
    {
        Ok(tip) => Ok(Some(tip)),
        Err(WorktreeError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Whether any registered worktree, the primary checkout included, has the
/// branch checked out.
pub fn branch_checked_out(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    cancel: &CancelToken,
) -> Result<bool, WorktreeError> {
    let entries = context.facts.fresh_inventory(context, cancel)?;
    checked_out_elsewhere(&entries, branch, None)
}
