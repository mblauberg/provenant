//! Removal of what a finished lane leaves behind: its worktree, local branch,
//! remote branch and remote-tracking ref. Every artefact reports one outcome.
//!
//! Only refs the ledger records as created by Provenant are candidates. A ref
//! it adopted, or never observed, is retained. Each deletion is leased on the
//! tip the caller just checked, so a ref that moved meanwhile is kept.
use super::*;
use provenant_os::fs::FileKind;
use provenant_records::{
    custody::{RemovalRecord, Subject, SubjectKind, WorktreeLease},
    warning::{CUSTODY_CHANGED, Warning, detected_source, warn},
};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtefactClass {
    Managed,
    Foreign,
}

/// One artefact's outcome. `name` is a worktree path or a full ref name;
/// `remote` names the bound remote for a remote branch or tracking ref.
/// `head` is the commit a removed ref held, which restores it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArtefactOutcome {
    pub artefact: Artefact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<ArtefactClass>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    pub outcome: ArtefactState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<GitOid>,
    /// Exact proof or uncertainty attached by the GC ref decision.
    #[serde(skip)]
    pub removal: Option<RemovalRecord>,
}

impl ArtefactOutcome {
    pub fn new(artefact: Artefact, name: impl Into<String>, outcome: ArtefactState) -> Self {
        Self {
            artefact,
            class: None,
            name: name.into(),
            remote: None,
            outcome,
            reason: None,
            head: None,
            removal: None,
        }
    }
    pub fn with_class(mut self, class: ArtefactClass) -> Self {
        self.class = Some(class);
        self
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

    pub fn with_removal(mut self, removal: RemovalRecord) -> Self {
        self.removal = Some(removal);
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
    landed::landed(
        &RepoGraph {
            context,
            repo: context.repo,
            cancel,
        },
        tip,
        root,
        chain,
    )
}

/// Whether the local object store holds `commit`, so a deleted ref that
/// pointed at it can be restored.
pub fn has_commit(
    context: &WorktreeContext<'_>,
    commit: GitOid,
    cancel: &CancelToken,
) -> Result<bool, WorktreeError> {
    Ok(RepoGraph {
        context,
        repo: context.repo,
        cancel,
    }
    .parents(commit)?
    .is_some())
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
/// `expected`. A branch observed checked out in the inventory or prepared
/// transaction check is kept. Git's reference transaction enforces the tip CAS.
pub fn delete_ref_leased(
    context: &WorktreeContext<'_>,
    reference: &GitRefName,
    expected: GitOid,
    cancel: &CancelToken,
) -> Result<RefDeletion, WorktreeError> {
    delete_ref_leased_when(context, reference, expected, cancel, &|| Ok(()))
}

pub fn delete_ref_leased_when(
    context: &WorktreeContext<'_>,
    reference: &GitRefName,
    expected: GitOid,
    cancel: &CancelToken,
    eligible: &dyn Fn() -> Result<(), WorktreeError>,
) -> Result<RefDeletion, WorktreeError> {
    let mut after_prepare = || {};
    delete_ref_leased_when_inner(
        context,
        reference,
        expected,
        cancel,
        eligible,
        &mut after_prepare,
    )
}

#[cfg(test)]
pub(super) fn delete_ref_leased_when_with_prepare_hook(
    context: &WorktreeContext<'_>,
    reference: &GitRefName,
    expected: GitOid,
    cancel: &CancelToken,
    eligible: &dyn Fn() -> Result<(), WorktreeError>,
    after_prepare: &mut dyn FnMut(),
) -> Result<RefDeletion, WorktreeError> {
    delete_ref_leased_when_inner(
        context,
        reference,
        expected,
        cancel,
        eligible,
        after_prepare,
    )
}

fn delete_ref_leased_when_inner(
    context: &WorktreeContext<'_>,
    reference: &GitRefName,
    expected: GitOid,
    cancel: &CancelToken,
    eligible: &dyn Fn() -> Result<(), WorktreeError>,
    after_prepare: &mut dyn FnMut(),
) -> Result<RefDeletion, WorktreeError> {
    let name = reference.as_str();
    if !(name.starts_with("refs/heads/") || name.starts_with("refs/remotes/")) {
        return Err(WorktreeError::InvalidRequest);
    }
    context.repo.revalidate()?;
    let mut stale = Vec::new();
    if name.starts_with("refs/heads/") {
        let fresh = context.facts.fresh_inventory(context, cancel)?;
        if checked_out_elsewhere(&fresh, reference, None)? {
            return Ok(RefDeletion::CheckedOut);
        }
        for entry in fresh
            .iter()
            .filter(|entry| entry.prunable && entry.branch.as_ref() == Some(reference))
        {
            if entry.locked {
                return Ok(RefDeletion::CheckedOut);
            }
            let Some(candidate) = stale_registration(context, entry, reference)? else {
                return Ok(RefDeletion::CheckedOut);
            };
            stale.push(candidate);
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
    eligible()?;
    let _ = context.facts.take_inventory(context);
    let mut prepared_check_result = None;
    let mut prepared_check = || {
        after_prepare();
        if let Err(error) =
            guard_destructive_branch(context.project_state, reference, "branch delete")
        {
            prepared_check_result = Some(Err(error));
            return false;
        }
        match branch_registered_as_checked_out(context.repo, reference, &stale) {
            Ok(checked_out) => {
                prepared_check_result = Some(Ok(checked_out));
                !checked_out
            }
            Err(error) => {
                prepared_check_result = Some(Err(error));
                false
            }
        }
    };
    let deleted = execute_git_prepared_ref_delete(
        context.executor,
        context.repo,
        context.utility,
        (reference, expected),
        context.mutation_budget,
        cancel,
        &mut prepared_check,
    )?;
    match prepared_check_result {
        Some(Err(error)) => return Err(error),
        Some(Ok(true)) => return Ok(RefDeletion::CheckedOut),
        Some(Ok(false)) | None => {}
    }
    Ok(if exit_code(&deleted)? == 0 {
        RefDeletion::Removed
    } else {
        RefDeletion::Changed
    })
}

/// Guard branch deletion against the canonical slot lease. The reservation
/// census classifies whether the lease holder is live; it is not itself a
/// substitute for the lease record that identifies branch custody.
pub fn guard_destructive_branch(
    project_state: &Directory,
    reference: &GitRefName,
    op: &str,
) -> Result<(), WorktreeError> {
    if !reference.as_str().starts_with("refs/heads/") {
        return Ok(());
    }
    let subject = Subject::new(SubjectKind::Branch, reference.as_str());
    let unknown = |detail: &str| {
        warn(
            Warning::new(
                CUSTODY_CHANGED,
                format!("{op} proceeds for {reference}: {detail} (source: worktree slot lease)"),
                detected_source(CUSTODY_CHANGED),
            )
            .subject(reference.as_str()),
        );
    };
    let listing = match provenant_supervisor::custody::list_writer_reservations(project_state) {
        Ok(listing) => listing,
        Err(error) => {
            unknown(&format!("writer custody could not be listed: {error}"));
            return Ok(());
        }
    };
    if !listing.issues.is_empty() {
        unknown("writer custody contains unreadable reservation evidence");
        return Ok(());
    }
    for reservation in listing
        .reservations
        .iter()
        .filter(|reservation| reservation.branch.as_deref() == Some(reference.as_str()))
    {
        match reservation.live {
            Some(false) => continue,
            None => {
                unknown(&format!(
                    "liveness of holder {} is unknown",
                    reservation.scope.attempt
                ));
                continue;
            }
            Some(true) => {}
        }

        let Some(slot_name) = reservation.custody_directory().and_then(Path::file_name) else {
            unknown("live branch holder has no worktree custody slot");
            continue;
        };
        let slot = match project_state.open_verified_directory(slot_name) {
            Ok(slot) => slot,
            Err(error) => {
                unknown(&format!(
                    "live branch custody slot could not be opened: {error}"
                ));
                continue;
            }
        };
        let bytes = match slot.read_stable(OsStr::new("lease.json"), 4096) {
            Ok(bytes) => bytes,
            Err(error) => {
                unknown(&format!("live branch lease could not be read: {error}"));
                continue;
            }
        };
        let lease: WorktreeLease = match serde_json::from_slice(&bytes) {
            Ok(lease) => lease,
            Err(error) => {
                unknown(&format!("live branch lease is malformed: {error}"));
                continue;
            }
        };
        if lease.holder != reservation.scope.attempt {
            unknown("live branch reservation does not match its slot lease");
            continue;
        }
        provenant_records::custody::guard_destructive(
            op,
            &subject,
            true,
            Some(reservation.scope.attempt),
        )
        .map_err(WorktreeError::CustodyCommand)?;
    }
    Ok(())
}

// The absence and exclusive occupancy proof stays held until the prepared ref
// transaction completes. A changed or newly created registration fails closed.
struct StaleRegistration {
    name: OsString,
    identity: provenant_os::fs::Metadata,
    gitdir: Vec<u8>,
    parent: Directory,
    parent_path: PathBuf,
    leaf: OsString,
    _lease: LockGuard,
}

fn stale_registration(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    branch: &GitRefName,
) -> Result<Option<StaleRegistration>, WorktreeError> {
    let (Some(parent_path), Some(leaf)) = (entry.path.parent(), entry.path.file_name()) else {
        return Ok(None);
    };
    let parent = match Directory::open_anchor(parent_path) {
        Ok(parent) => parent,
        Err(_) => return Ok(None),
    };
    if parent.current_path()? != parent_path {
        return Ok(None);
    }
    match parent.metadata_at(leaf, false) {
        Err(error) if error.is_not_found() => {}
        _ => return Ok(None),
    }
    let lease = match cache_writer_guard(context.project_state, &entry.path) {
        Ok(lease) => lease,
        Err(_) => return Ok(None),
    };
    let registrations = match context
        .repo
        .common_dir()
        .open_verified_directory(OsStr::new("worktrees"))
    {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let expected = entry.path.join(".git");
    let expected = expected.as_os_str().as_bytes();
    for (index, name) in registrations.entries()?.enumerate() {
        if index >= MAX_WORKTREES {
            return Err(WorktreeError::InvalidOutput);
        }
        let name = name?;
        let identity = registrations.metadata_at(&name, false)?;
        if identity.kind != FileKind::Directory {
            return Err(WorktreeError::InvalidOutput);
        }
        let admin = registrations.open_verified_directory(&name)?;
        if !head_targets_branch(&admin, branch)? {
            continue;
        }
        let gitdir = admin.read_stable(OsStr::new("gitdir"), 4096)?;
        if gitdir.strip_suffix(b"\n").unwrap_or(&gitdir) == expected {
            return Ok(Some(StaleRegistration {
                name,
                identity,
                gitdir,
                parent,
                parent_path: parent_path.to_owned(),
                leaf: leaf.to_owned(),
                _lease: lease,
            }));
        }
    }
    Ok(None)
}

/// Check the checked-out branch refs from Git's stable worktree HEAD files
/// while `update-ref` is prepared. Any existing worktree registration that has
/// already recorded this branch is observed, preserving the branch.
fn branch_registered_as_checked_out(
    repo: &VerifiedRepo,
    branch: &GitRefName,
    stale: &[StaleRegistration],
) -> Result<bool, WorktreeError> {
    if head_targets_branch(repo.common_dir(), branch)? {
        return Ok(true);
    }
    let registrations_name = OsStr::new("worktrees");
    let registrations_metadata = match repo.common_dir().metadata_at(registrations_name, false) {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if registrations_metadata.kind != FileKind::Directory {
        return Err(WorktreeError::InvalidOutput);
    }
    let registrations = repo
        .common_dir()
        .open_verified_directory(registrations_name)?;
    let mut count = 0usize;
    for name in registrations.entries()? {
        let name = name?;
        count = count.saturating_add(1);
        if count > MAX_WORKTREES {
            return Err(WorktreeError::InvalidOutput);
        }
        if registrations.metadata_at(&name, false)?.kind != FileKind::Directory {
            return Err(WorktreeError::InvalidOutput);
        }
        let worktree = registrations.open_verified_directory(&name)?;
        if head_targets_branch(&worktree, branch)? {
            let Some(candidate) = stale.iter().find(|candidate| candidate.name == name) else {
                return Ok(true);
            };
            if !candidate
                .identity
                .same_identity(&registrations.metadata_at(&name, false)?)
                || worktree.read_stable(OsStr::new("gitdir"), 4096)? != candidate.gitdir
                || candidate.parent.current_path()? != candidate.parent_path
                || !matches!(candidate.parent.metadata_at(&candidate.leaf, false), Err(error) if error.is_not_found())
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn head_targets_branch(git_dir: &Directory, branch: &GitRefName) -> Result<bool, WorktreeError> {
    let bytes = git_dir.read_stable(OsStr::new("HEAD"), 4096)?;
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    if let Some(reference) = bytes.strip_prefix(b"ref: ") {
        let reference = std::str::from_utf8(reference).map_err(|_| WorktreeError::InvalidOutput)?;
        let reference = GitRefName::new(reference).map_err(|_| WorktreeError::InvalidOutput)?;
        return Ok(reference.as_str() == branch.as_str());
    }
    let oid = std::str::from_utf8(bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    GitOid::parse(oid).map_err(|_| WorktreeError::InvalidOutput)?;
    Ok(false)
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
