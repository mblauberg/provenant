//! The GC worktree selection, shared by merged removal and declared output.
use super::*;
use provenant_config::RetentionIssue;
use provenant_os::fs::{FileKind, PayloadTree, entry_names, temporary_name};
use provenant_records::custody::{
    LandedEvidence, LandingPredicate, LandingSource, RefEndpointIdentity, RemovalEvidence,
    RemovalOutcome, RemovalRecord, RemovalUncertainty, RetirementDecision, RetirementRecord,
    RetirementStep, RetirementStepState, Subject, SubjectKind,
};
use std::collections::HashSet;

/// One budget covers every output candidate in a preview. Applied collection
/// continues to use the full proof before mutation.
struct PreviewWalk {
    started: std::time::Instant,
    remaining: std::cell::Cell<usize>,
    enabled: bool,
}

impl PreviewWalk {
    #[allow(clippy::disallowed_methods)]
    fn new(enabled: bool) -> Self {
        Self {
            started: std::time::Instant::now(),
            remaining: std::cell::Cell::new(20_000),
            enabled,
        }
    }

    fn step(&self) -> Result<(), FsError> {
        if self.enabled {
            let remaining = self.remaining.get();
            if remaining == 0 || self.started.elapsed() >= std::time::Duration::from_millis(500) {
                return Err(FsError::EntryLimit { limit: 20_000 });
            }
            self.remaining.set(remaining - 1);
        }
        Ok(())
    }

    fn newest(
        &self,
        directory: &Directory,
        depth: usize,
        repositories: bool,
    ) -> Result<i128, FsError> {
        self.step()?;
        if depth >= 64 {
            return Err(FsError::EntryLimit { limit: 64 });
        }
        if repositories && contains_repository(directory)? {
            return Err(FsError::RaceRefused {
                name: ".git".into(),
            });
        }
        let mut newest = directory.metadata()?.modified_ns;
        for name in entry_names(directory, if self.enabled { 20_000 } else { 100_000 })? {
            self.step()?;
            if name == ".git" {
                continue;
            }
            let metadata = directory.metadata_at(&name, false)?;
            newest = newest.max(metadata.modified_ns);
            if metadata.kind == FileKind::Directory {
                newest = newest.max(self.newest(
                    &directory.open_verified_directory(&name)?,
                    depth + 1,
                    repositories,
                )?);
            }
        }
        Ok(newest)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionMode {
    Outputs,
    Worktrees,
}

#[derive(Clone, Copy)]
pub struct WorktreeGcPolicy<'a> {
    pub upstream: Option<GitOid>,
    pub mode: CollectionMode,
    pub proof: Option<&'a provenant_records::custody::LandedEvidence>,
    pub upstream_reference: Option<&'a str>,
    pub terminal: &'a dyn Fn(&Path) -> bool,
    pub no_live_lanes: &'a dyn Fn(&Path) -> bool,
    pub regenerable: &'a [String],
    pub keep: &'a [String],
    pub written_before_ns: i128,
    pub dry_run: bool,
    pub include_foreign: bool,
    /// The sole durable retirement writer.
    pub retirement_port: Option<&'a dyn RetirementPort>,
    /// Current custody, forge, and integration observations used to decide.
    pub retirement_feeds: Option<&'a dyn RetirementFeeds>,
    pub integration_patterns: &'a [String],
    pub excluded_paths: &'a [String],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgeObservation {
    Absent,
    Open {
        base: String,
    },
    Closed {
        head: GitOid,
    },
    Merged {
        pr: String,
        head: GitOid,
        merge_commit: Option<GitOid>,
    },
    Unknown(String),
}
/// Live application feeds used to decide whether a worktree may be retired.
/// Durable state and step outcomes are exclusively owned by `RetirementPort`.
pub trait RetirementFeeds {
    fn authority(&self) -> Result<provenant_core::ActorId, String>;
    fn custody(&self, lane: &WorktreeInfo) -> Result<(), String>;
    fn forge(&self, branch: &GitRefName) -> ForgeObservation;
    fn integration_bases(&self) -> Vec<(String, GitOid)>;
    fn remote_guard(
        &self,
        context: &WorktreeContext<'_>,
        branch: &str,
        tip: GitOid,
        min_days: u64,
    ) -> Result<(), String>;
}

#[derive(Debug, Serialize)]
pub struct CandidateReport {
    pub path: PathBuf,
    pub class: &'static str,
    pub action: &'static str,
    pub tip: GitOid,
    pub detail: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct WorktreeGcReport {
    pub dry_run: bool,
    pub worktrees: RemoveOutcome,
    pub output: Vec<OutputEntry>,
    /// Declared ignored output in worktrees without a terminal lane.
    pub idle_output: Vec<OutputEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_error: Option<String>,
    /// One outcome per ref the ledger records as created: removed once
    /// landed with an unchanged recorded tip, otherwise retained with its reason.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<super::ArtefactOutcome>,
    /// Records are lifted to the enclosing GC output alongside ref outcomes.
    #[serde(skip)]
    pub removals: Vec<RemovalRecord>,
    pub candidates: Vec<CandidateReport>,
    pub first_foreign_preview: bool,
    pub bytes: u64,
    pub caches: Vec<crate::cache::CacheGcEntry>,
    pub byte_errors: Vec<RetentionIssue>,
    pub cache_errors: Vec<RetentionIssue>,
}

#[derive(Debug, Serialize)]
pub struct OutputEntry {
    pub bytes: u64,
    pub path: PathBuf,
    pub removed: bool,
    pub reason: String,
}

pub fn branch_candidate(
    context: &WorktreeContext<'_>,
    reference: &GitRefName,
    tip: GitOid,
) -> WorktreeInfo {
    WorktreeInfo {
        warnings: Vec::new(),
        path: context.repo.root_path().into(),
        branch: Some(reference.as_str().into()),
        head: tip,
        dirty: WorktreeCondition::Clean,
        owner: Ok(None),
        primary: true,
        occupancy: Occupancy::Available,
    }
}

fn kept_landing_record(
    project: provenant_core::ProjectId,
    lane: &WorktreeInfo,
    cause: RemovalUncertainty,
) -> RemovalRecord {
    RemovalRecord {
        project,
        subject: Subject::new(SubjectKind::Worktree, lane.path.to_string_lossy()),
        tip: lane.head,
        outcome: RemovalOutcome::Kept { cause },
    }
}

fn canonical_worktree_subject(verified: &VerifiedWorktree) -> Result<Subject, WorktreeError> {
    let canonical = canonicalize(verified.path())?;
    let path = canonical.to_str().ok_or(WorktreeError::InvalidRequest)?;
    Ok(Subject::new(SubjectKind::Worktree, path))
}

fn pending_retirement_for_candidate(
    port: &dyn RetirementPort,
    project: provenant_core::ProjectId,
    worktree: &Subject,
    reference: &provenant_records::custody::RefOwnership,
    expected_tip: GitOid,
    decision: &RetirementDecision,
) -> Result<Option<RetirementRecord>, WorktreeError> {
    let reference_subject = reference
        .custody_subject()
        .map_err(|_| WorktreeError::InvalidRequest)?;
    let mut found: Option<RetirementRecord> = None;
    for subject in [worktree, &reference_subject] {
        let Some(record) = port.retirement_for_subject(project, subject)? else {
            continue;
        };
        if record.project != project
            || record.expected_tip != expected_tip
            || !record.related_subjects.contains(worktree)
            || !record.owns_subject(worktree)
            || !record
                .reference
                .matches_ownership(reference)
                .map_err(|_| WorktreeError::InvalidRequest)?
            || !record.decision.permits_replay(decision)
            || !decision.valid_for(&record.related_subjects, expected_tip)
            || !record
                .decision
                .valid_for(&record.related_subjects, expected_tip)
            || !record.valid_evidence()
        {
            return Err(WorktreeError::IdentityChanged);
        }
        if found
            .as_ref()
            .is_some_and(|previous| previous.id != record.id)
        {
            return Err(WorktreeError::IdentityChanged);
        }
        found = Some(record);
    }
    Ok(found)
}

fn refuse_unqualified_pending_retirement(
    port: &dyn RetirementPort,
    project: provenant_core::ProjectId,
    worktree: &Subject,
) -> Result<(), WorktreeError> {
    if port.retirement_for_subject(project, worktree)?.is_some() {
        return Err(WorktreeError::IdentityChanged);
    }
    Ok(())
}

fn validate_forge_head(
    feeds: &dyn RetirementFeeds,
    branch: &GitRefName,
    evidence: &LandedEvidence,
) -> Result<(), WorktreeError> {
    let LandingSource::ForgeMerged {
        pr,
        head,
        merge_commit: None,
    } = &evidence.source
    else {
        return Ok(());
    };
    match feeds.forge(branch) {
        ForgeObservation::Merged {
            pr: current_pr,
            head: current_head,
            ..
        } if current_pr.as_str() == pr.as_str() && current_head == *head => Ok(()),
        _ => Err(WorktreeError::IdentityChanged),
    }
}

fn refresh_owned_reference(
    context: &WorktreeContext<'_>,
    verified: &VerifiedWorktree,
    lane: &WorktreeInfo,
    branch: &GitRefName,
    reference: &provenant_records::custody::RefOwnership,
) -> Result<(), WorktreeError> {
    let expected = lane
        .owner
        .as_ref()
        .map_err(|_| WorktreeError::UnknownWriter)?;
    let current = load_owner(verified)?;
    if &current != expected {
        return Err(WorktreeError::IdentityChanged);
    }
    let Some(owner) = current else {
        return Ok(());
    };
    if owner.branch != branch.short_name() {
        return Err(WorktreeError::IdentityChanged);
    }
    if owner.reference.as_ref() != Some(reference) {
        super::refresh_owner_reference_while_held(context, verified, &owner, reference.clone())?;
    }
    Ok(())
}

fn retirement_reference(
    context: &WorktreeContext<'_>,
    port: &dyn RetirementPort,
    lane: &WorktreeInfo,
    verified: &VerifiedWorktree,
    decision: &RetirementDecision,
    cancel: &CancelToken,
) -> Result<provenant_records::custody::RefOwnership, WorktreeError> {
    let project = context.binding.project_id();
    let ledger = RefLedger::for_context(context)?;
    let worktree = canonical_worktree_subject(verified)?;
    let branch = lane
        .branch
        .as_deref()
        .map(GitRefName::new)
        .transpose()
        .map_err(|_| WorktreeError::InvalidOutput)?;
    if let Some(branch) = branch {
        if context
            .facts
            .resolve_commit(context, branch.clone(), cancel)?
            != lane.head
        {
            return Err(WorktreeError::IdentityChanged);
        }
        let owner = lane
            .owner
            .as_ref()
            .map_err(|_| WorktreeError::UnknownWriter)?
            .as_ref();
        if owner.is_some_and(|owner| owner.branch != branch.short_name()) {
            return Err(WorktreeError::IdentityChanged);
        }
        let existing = ledger.load(&RefEndpoint::Local, &branch)?;
        let owner_reference =
            owner
                .and_then(|owner| owner.reference.as_ref())
                .filter(|reference| {
                    reference.repository == project
                        && reference.endpoint == RefEndpoint::Local
                        && reference.reference == branch.as_str()
                });
        let prior = existing
            .as_ref()
            .map(|record| record.ownership())
            .or_else(|| owner_reference.cloned());
        if prior.is_none() {
            refuse_unqualified_pending_retirement(port, project, &worktree)?;
        }
        if let Some(prior) = &prior
            && let Some(pending) = pending_retirement_for_candidate(
                port, project, &worktree, prior, lane.head, decision,
            )?
        {
            let record = existing
                .as_ref()
                .filter(|record| record.tip == lane.head)
                .ok_or(WorktreeError::IdentityChanged)?;
            if !pending
                .reference
                .matches_ownership(&record.ownership())
                .map_err(|_| WorktreeError::InvalidRequest)?
                || pending.expected_tip != record.tip
            {
                return Err(WorktreeError::IdentityChanged);
            }
            refresh_owned_reference(context, verified, lane, &branch, &record.ownership())?;
            return Ok(record.ownership());
        }

        let record = match existing {
            Some(record) if record.tip == lane.head => record,
            _ => ledger.observe_local_branch(&branch, lane.head, context.clock, cancel)?,
        };
        let reference = record.ownership();
        if pending_retirement_for_candidate(
            port, project, &worktree, &reference, lane.head, decision,
        )?
        .is_some()
        {
            return Err(WorktreeError::IdentityChanged);
        }
        refresh_owned_reference(context, verified, lane, &branch, &reference)?;
        Ok(reference)
    } else {
        let canonical = canonicalize(verified.path())?;
        let existing = ledger.load_worktree_head(&canonical)?;
        if existing.is_none() {
            refuse_unqualified_pending_retirement(port, project, &worktree)?;
        }
        if let Some(record) = &existing
            && let Some(pending) = pending_retirement_for_candidate(
                port,
                project,
                &worktree,
                &record.ownership(),
                lane.head,
                decision,
            )?
        {
            if record.tip != lane.head
                || !pending
                    .reference
                    .matches_ownership(&record.ownership())
                    .map_err(|_| WorktreeError::InvalidRequest)?
            {
                return Err(WorktreeError::IdentityChanged);
            }
            return Ok(record.ownership());
        }
        let record = match existing {
            Some(record) if record.tip == lane.head => record,
            _ => ledger.record_worktree_head(&canonical, lane.head, context.clock, cancel)?,
        };
        if pending_retirement_for_candidate(
            port,
            project,
            &worktree,
            &record.ownership(),
            lane.head,
            decision,
        )?
        .is_some()
        {
            return Err(WorktreeError::IdentityChanged);
        }
        Ok(record.ownership())
    }
}

/// Begin the one stored retirement lifecycle for a worktree and prepare its
/// destructive step. The caller holds repository and writer custody while this
/// observes the current ref generation and refreshes any owner link.
pub(super) fn begin_worktree_retirement(
    context: &WorktreeContext<'_>,
    port: &dyn RetirementPort,
    lane: &WorktreeInfo,
    verified: &VerifiedWorktree,
    decision: RetirementDecision,
    cancel: &CancelToken,
) -> Result<RetirementRecord, WorktreeError> {
    let worktree = canonical_worktree_subject(verified)?;
    if !decision.valid_for(std::slice::from_ref(&worktree), lane.head) {
        return Err(WorktreeError::InvalidRequest);
    }
    let reference = retirement_reference(context, port, lane, verified, &decision, cancel)?;
    let worktree_path = match &reference.endpoint {
        RefEndpoint::Worktree { path } => path.clone(),
        _ => worktree.id.clone(),
    };
    let worktree = Subject::new(SubjectKind::Worktree, worktree_path);
    begin_retirement_step(
        context,
        port,
        reference,
        lane.head,
        vec![worktree.clone()],
        decision,
        RetirementStep::Worktree { subject: worktree },
    )
}

pub(super) fn begin_reference_retirement(
    context: &WorktreeContext<'_>,
    port: &dyn RetirementPort,
    branch: &GitRefName,
    tip: GitOid,
    decision: RetirementDecision,
    cancel: &CancelToken,
) -> Result<RetirementRecord, WorktreeError> {
    if context
        .facts
        .resolve_commit(context, branch.clone(), cancel)?
        != tip
    {
        return Err(WorktreeError::IdentityChanged);
    }
    if !decision.valid_for(&[], tip) {
        return Err(WorktreeError::InvalidRequest);
    }
    let project = context.binding.project_id();
    let ledger = RefLedger::for_context(context)?;
    let existing = ledger.load(&RefEndpoint::Local, branch)?;
    if existing.is_none()
        && port.pending_retirements(project)?.iter().any(|record| {
            record.project == project
                && record.reference.identity.endpoint == RefEndpointIdentity::Local
                && record.reference.identity.reference == branch.as_str()
        })
    {
        return Err(WorktreeError::IdentityChanged);
    }
    if let Some(previous) = &existing {
        let ownership = previous.ownership();
        let subject = ownership
            .custody_subject()
            .map_err(|_| WorktreeError::InvalidRequest)?;
        if let Some(record) = port.retirement_for_subject(project, &subject)? {
            if record.project != project
                || record.expected_tip != tip
                || !record.related_subjects.is_empty()
                || !record.owns_subject(&subject)
                || !record
                    .reference
                    .matches_ownership(&ownership)
                    .map_err(|_| WorktreeError::InvalidRequest)?
                || !record.decision.permits_replay(&decision)
                || !record.decision.valid_for(&record.related_subjects, tip)
                || !record.valid_evidence()
                || previous.tip != tip
            {
                return Err(WorktreeError::IdentityChanged);
            }
            return begin_retirement_step(
                context,
                port,
                ownership,
                tip,
                Vec::new(),
                decision,
                RetirementStep::Reference,
            );
        }
    }
    let record = match existing {
        Some(record) if record.tip == tip => record,
        _ => ledger.observe_local_branch(branch, tip, context.clock, cancel)?,
    };
    let reference = record.ownership();
    let subject = reference
        .custody_subject()
        .map_err(|_| WorktreeError::InvalidRequest)?;
    if port.retirement_for_subject(project, &subject)?.is_some() {
        return Err(WorktreeError::IdentityChanged);
    }
    begin_retirement_step(
        context,
        port,
        reference,
        tip,
        Vec::new(),
        decision,
        RetirementStep::Reference,
    )
}

fn begin_retirement_step(
    context: &WorktreeContext<'_>,
    port: &dyn RetirementPort,
    reference: provenant_records::custody::RefOwnership,
    expected_tip: GitOid,
    related_subjects: Vec<Subject>,
    decision: RetirementDecision,
    step: RetirementStep,
) -> Result<RetirementRecord, WorktreeError> {
    let record = port
        .begin_retirement(RetirementIntent {
            project: context.binding.project_id(),
            related_subjects: related_subjects.clone(),
            reference: reference.clone(),
            expected_tip,
            decision: decision.clone(),
        })?
        .ok_or(WorktreeError::RegistrationUnconfirmed)?;
    if record.project != context.binding.project_id()
        || record.expected_tip != expected_tip
        || record.related_subjects != related_subjects
        || !record.decision.permits_replay(&decision)
        || !decision.valid_for(&record.related_subjects, expected_tip)
        || !record
            .reference
            .matches_ownership(&reference)
            .map_err(|_| WorktreeError::InvalidRequest)?
        || !record.valid_evidence()
    {
        return Err(WorktreeError::IdentityChanged);
    }
    if record.outcomes.iter().any(|outcome| {
        outcome.step == step && matches!(&outcome.state, RetirementStepState::Completed { .. })
    }) {
        return Err(WorktreeError::RegistrationUnconfirmed);
    }
    prepare_retirement_step(port, &record, step)
}

pub(super) fn finish_worktree_retirement(
    port: &dyn RetirementPort,
    record: &RetirementRecord,
    path: &Path,
    completion: RemovalEvidence,
) -> Result<RetirementRecord, WorktreeError> {
    let subject = record
        .related_subjects
        .iter()
        .find(|subject| subject.kind == SubjectKind::Worktree)
        .filter(|subject| Path::new(&subject.id) == path)
        .cloned()
        .ok_or(WorktreeError::IdentityChanged)?;
    let step = RetirementStep::Worktree { subject };
    if !record.accepts_completion_evidence(&step, &completion) {
        return Err(WorktreeError::InvalidRequest);
    }
    update_retirement_step(
        port,
        record,
        step,
        RetirementStepState::Completed {
            evidence: completion,
        },
    )
}

pub(super) fn fail_worktree_retirement(
    port: &dyn RetirementPort,
    record: &RetirementRecord,
    path: &Path,
    reason: String,
) -> Result<RetirementRecord, WorktreeError> {
    let subject = record
        .related_subjects
        .iter()
        .find(|subject| subject.kind == SubjectKind::Worktree)
        .filter(|subject| Path::new(&subject.id) == path)
        .cloned()
        .ok_or(WorktreeError::IdentityChanged)?;
    update_retirement_step(
        port,
        record,
        RetirementStep::Worktree { subject },
        RetirementStepState::Unknown { reason },
    )
}

pub(super) fn finish_reference_retirement(
    port: &dyn RetirementPort,
    record: &RetirementRecord,
    removed: bool,
    completion: RemovalEvidence,
) -> Result<RetirementRecord, WorktreeError> {
    if !record.decision.removes_reference() {
        return Ok(record.clone());
    }
    let step = RetirementStep::Reference;
    if !record.accepts_completion_evidence(&step, &completion) {
        return Err(WorktreeError::InvalidRequest);
    }
    let state = if removed {
        RetirementStepState::Completed {
            evidence: completion,
        }
    } else {
        RetirementStepState::Unknown {
            reason: "reference removal was not confirmed".into(),
        }
    };
    update_retirement_step(port, record, step, state)
}

fn path_is_absent(path: &Path) -> Result<bool, WorktreeError> {
    let Some(parent) = path.parent() else {
        return Ok(false);
    };
    let Some(name) = path.file_name() else {
        return Ok(false);
    };
    let directory = match Directory::open_anchor(parent) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    match directory.metadata_at(name, false) {
        Ok(_) => Ok(false),
        Err(error) if error.is_not_found() => Ok(true),
        Err(error) => Err(error.into()),
    }
}

/// Recover only outcomes whose absence is proved by fresh repository and
/// filesystem observations. Present candidates stay pending for the R3
/// selector to re-evaluate with current retention evidence.
pub fn resume_pending_retirements(
    context: &WorktreeContext<'_>,
    port: &dyn RetirementPort,
    cancel: &CancelToken,
) -> Result<Vec<RetirementRecord>, WorktreeError> {
    validate_binding(context)?;
    let project = context.binding.project_id();
    let inventory = context.facts.fresh_inventory(context, cancel)?;
    let pending = port.pending_retirements(project)?;
    let mut updated = Vec::with_capacity(pending.len());
    for mut record in pending {
        if record.project != project || !record.valid_evidence() {
            return Err(WorktreeError::InvalidOutput);
        }
        for outcome in record.outcomes.clone() {
            match outcome.step {
                RetirementStep::Worktree { ref subject } => {
                    if matches!(outcome.state, RetirementStepState::Completed { .. })
                        || subject.kind != SubjectKind::Worktree
                    {
                        continue;
                    }
                    let path = Path::new(&subject.id);
                    let registered = inventory.iter().any(|entry| entry.path == path);
                    if registered || !path_is_absent(path)? {
                        continue;
                    }
                    let evidence = record.decision.evidence().clone();
                    if record.accepts_completion_evidence(&outcome.step, &evidence) {
                        record = update_retirement_step(
                            port,
                            &record,
                            outcome.step,
                            RetirementStepState::Completed { evidence },
                        )?;
                    }
                }
                RetirementStep::Reference => {
                    if matches!(outcome.state, RetirementStepState::Completed { .. })
                        || !outcome.step.prerequisites_complete(&record.outcomes)
                    {
                        continue;
                    }
                    let RefEndpointIdentity::Local = &record.reference.identity.endpoint else {
                        continue;
                    };
                    let reference = GitRefName::new(&record.reference.identity.reference)
                        .map_err(|_| WorktreeError::InvalidOutput)?;
                    if !matches!(
                        context.facts.resolve_commit(context, reference, cancel),
                        Err(WorktreeError::NotFound)
                    ) {
                        continue;
                    }
                    let evidence = record.decision.evidence().clone();
                    if record.accepts_completion_evidence(&outcome.step, &evidence) {
                        record = update_retirement_step(
                            port,
                            &record,
                            RetirementStep::Reference,
                            RetirementStepState::Completed { evidence },
                        )?;
                    }
                }
            }
        }
        updated.push(record);
    }
    Ok(updated)
}

/// Recheck each candidate under repository and writer custody. Failures keep
/// that entry, rather than aborting cleanup of unrelated worktrees.
pub fn collect(
    context: &WorktreeContext<'_>,
    lanes: &[WorktreeInfo],
    policy: &WorktreeGcPolicy<'_>,
    cancel: &CancelToken,
) -> Result<WorktreeGcReport, WorktreeError> {
    collect_impl(context, lanes, policy, cancel)
}

/// Revalidate a caller-supplied landing proof against the exact live worktree
/// head and pinned target immediately before retirement begins.
pub(super) fn verify_retirement_landing_evidence(
    context: &WorktreeContext<'_>,
    head: GitOid,
    expected_target: GitOid,
    evidence: &LandedEvidence,
    cancel: &CancelToken,
) -> Result<bool, WorktreeError> {
    use LandingSource::{ForgeMerged, IntegrationTarget, RetainedRef, TreeEquivalent};

    let target = LandingTarget {
        reference: None,
        tip: expected_target,
    };
    match &evidence.source {
        IntegrationTarget { reference, commit } | RetainedRef { reference, commit } => {
            if *commit != expected_target {
                return Ok(false);
            }
            let Some(name) = evidence_ref_name(reference) else {
                return Ok(false);
            };
            if context
                .facts
                .resolve_commit(context, name.clone(), cancel)?
                != *commit
            {
                return Ok(false);
            }
            Ok(matches!(
                landed::landing_on(
                    context,
                    head,
                    &LandingTarget {
                        reference: Some(name),
                        tip: *commit,
                    },
                    cancel,
                )?,
                Landing::Landed(_)
            ))
        }
        TreeEquivalent {
            reference,
            head: proof_head,
            target: proof_target,
            tree,
        } => {
            if *proof_head != head || *proof_target != expected_target {
                return Ok(false);
            }
            let Some(name) = evidence_ref_name(reference) else {
                return Ok(false);
            };
            if context.facts.resolve_commit(context, name, cancel)? != *proof_target {
                return Ok(false);
            }
            let head_tree = resolve_commit_tree(context, head, cancel)?;
            let target_tree = resolve_commit_tree(context, expected_target, cancel)?;
            Ok(head_tree == *tree && target_tree == *tree)
        }
        ForgeMerged {
            head: proof_head,
            merge_commit,
            ..
        } => {
            if *proof_head != head {
                return Ok(false);
            }
            if let Some(merge_commit) = merge_commit {
                let merge = LandingTarget {
                    reference: None,
                    tip: *merge_commit,
                };
                if !matches!(
                    landed::landing_on(context, head, &merge, cancel)?,
                    Landing::Landed(_)
                ) || !matches!(
                    landed::landing_on(context, *merge_commit, &target, cancel)?,
                    Landing::Landed(_)
                ) {
                    return Ok(false);
                }
                Ok(true)
            } else {
                Ok(matches!(
                    landed::landing_on(context, head, &target, cancel)?,
                    Landing::Landed(_)
                ))
            }
        }
    }
}

fn evidence_ref_name(reference: &str) -> Option<GitRefName> {
    if let Some(branch) = reference.strip_prefix("refs/heads/") {
        return GitRefName::branch(branch).ok();
    }
    reference
        .strip_prefix("refs/remotes/")
        .and_then(|remote_tracking| GitRefName::remote_tracking(remote_tracking).ok())
}

fn resolve_commit_tree(
    context: &WorktreeContext<'_>,
    commit: GitOid,
    cancel: &CancelToken,
) -> Result<GitOid, WorktreeError> {
    let outcome = run_git(
        context,
        context.repo,
        GitRead::ResolveCommitTree { oid: commit },
        cancel,
    )?;
    if exit_code(&outcome)? != 0 {
        return Err(WorktreeError::NotFound);
    }
    parse_oid(trim_ascii(&outcome.stdout.bytes))
}

pub(super) fn update_retirement_step(
    port: &dyn RetirementPort,
    record: &provenant_records::custody::RetirementRecord,
    step: RetirementStep,
    state: RetirementStepState,
) -> Result<provenant_records::custody::RetirementRecord, WorktreeError> {
    let current = record
        .outcomes
        .iter()
        .find(|outcome| outcome.step == step)
        .ok_or(WorktreeError::RegistrationUnconfirmed)?;
    if current.state == state || matches!(&current.state, RetirementStepState::Completed { .. }) {
        return Ok(record.clone());
    }
    let mut current_record = record.clone();
    if matches!(
        &current.state,
        RetirementStepState::Refused { .. } | RetirementStepState::Unknown { .. }
    ) && !matches!(&state, RetirementStepState::Pending)
    {
        current_record =
            port.advance_retirement(&current_record, step.clone(), RetirementStepState::Pending)?;
    }
    port.advance_retirement(&current_record, step, state)
}

/// Reopen an uncertain step before retrying its destructive action. The
/// returned revision must be used for the subsequent completion outcome.
pub(super) fn prepare_retirement_step(
    port: &dyn RetirementPort,
    record: &provenant_records::custody::RetirementRecord,
    step: RetirementStep,
) -> Result<provenant_records::custody::RetirementRecord, WorktreeError> {
    let current = record
        .outcomes
        .iter()
        .find(|outcome| outcome.step == step)
        .ok_or(WorktreeError::RegistrationUnconfirmed)?;
    match &current.state {
        RetirementStepState::Pending => Ok(record.clone()),
        RetirementStepState::Refused { .. } | RetirementStepState::Unknown { .. } => {
            port.advance_retirement(record, step, RetirementStepState::Pending)
        }
        RetirementStepState::Completed { .. } => Err(WorktreeError::RegistrationUnconfirmed),
    }
}

fn allocated_tree(directory: &Directory, depth: usize) -> Result<u64, FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    let mut bytes = directory.metadata()?.allocated;
    for name in entry_names(directory, 100_000)? {
        let metadata = directory.metadata_at(&name, false)?;
        bytes = bytes.saturating_add(if metadata.kind == FileKind::Directory {
            allocated_tree(&directory.open_verified_directory(&name)?, depth + 1)?
        } else {
            metadata.allocated
        });
    }
    Ok(bytes)
}

fn observed_entry_time(root: &Directory, path: &Path) -> Result<Option<i128>, FsError> {
    let mut directory = root.try_clone()?;
    let components: Vec<_> = path.components().collect();
    let Some((last, parents)) = components.split_last() else {
        return Ok(None);
    };
    for component in parents {
        match directory.open_verified_directory(component.as_os_str()) {
            Ok(child) => directory = child,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    match directory.metadata_at(last.as_os_str(), false) {
        Ok(metadata) => Ok(Some(metadata.modified_ns)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error),
    }
}

fn latest_git_activity(
    repo: &VerifiedRepo,
    branch: Option<&GitRefName>,
    committed: i128,
) -> Result<i128, FsError> {
    let mut newest = committed;
    for name in ["HEAD", "index"] {
        if let Some(time) = observed_entry_time(repo.private_dir(), Path::new(name))? {
            newest = newest.max(time);
        }
    }
    let mut paths = vec![PathBuf::from("packed-refs")];
    if let Some(branch) = branch {
        paths.push(PathBuf::from(branch.as_str()));
        paths.push(Path::new("logs").join(branch.as_str()));
    }
    for path in paths {
        if let Some(time) = observed_entry_time(repo.common_dir(), &path)? {
            newest = newest.max(time);
        }
    }
    Ok(newest)
}

fn foreign_preview_marker(context: &WorktreeContext<'_>) -> String {
    format!(
        "foreign-preview-{}-{}",
        context.repo.common_identity().device,
        context.repo.common_identity().inode
    )
}
pub fn first_foreign_pass(context: &WorktreeContext<'_>) -> Result<bool, WorktreeError> {
    match context
        .project_state
        .metadata_at(OsStr::new(&foreign_preview_marker(context)), false)
    {
        Ok(_) => Ok(false),
        Err(error) if error.is_not_found() => Ok(true),
        Err(error) => Err(error.into()),
    }
}
pub fn acknowledge_foreign_preview(context: &WorktreeContext<'_>) -> Result<(), WorktreeError> {
    context
        .project_state
        .atomic_write(
            OsStr::new(&foreign_preview_marker(context)),
            ".foreign-preview-",
            b"{\"schema_version\":2}\n",
            provenant_os::fs::PublishMode::Replace,
        )?
        .require_durable()
        .map_err(|_| WorktreeError::InvalidOutput)
}

enum SelectedRetirement {
    Subsumed(LandedEvidence),
    Idle,
}

impl SelectedRetirement {
    fn completion_evidence(&self, tip: GitOid) -> RemovalEvidence {
        match self {
            Self::Subsumed(evidence) => RemovalEvidence::Landing {
                evidence: evidence.clone(),
            },
            Self::Idle => RemovalEvidence::IdleRetirement { tip },
        }
    }

    fn decision(
        &self,
        authority: provenant_core::ActorId,
        branch: Option<&GitRefName>,
        tip: GitOid,
    ) -> RetirementDecision {
        match self {
            Self::Subsumed(evidence)
                if branch.is_some()
                    && evidence.predicate == LandingPredicate::Landed
                    && matches!(
                        &evidence.source,
                        LandingSource::IntegrationTarget { .. } | LandingSource::ForgeMerged { .. }
                    ) =>
            {
                RetirementDecision::Discard {
                    authority,
                    evidence: self.completion_evidence(tip),
                }
            }
            Self::Subsumed(_) => RetirementDecision::RetireWorktree {
                authority,
                evidence: self.completion_evidence(tip),
            },
            Self::Idle => RetirementDecision::RetireWorktree {
                authority,
                evidence: self.completion_evidence(tip),
            },
        }
    }

    fn removes_reference(&self, branch: Option<&GitRefName>) -> bool {
        branch.is_some()
            && matches!(
                self,
                Self::Subsumed(evidence)
                    if evidence.predicate == LandingPredicate::Landed
                        && matches!(
                            &evidence.source,
                            LandingSource::IntegrationTarget { .. }
                                | LandingSource::ForgeMerged { .. }
                        )
            )
    }
}

fn collect_impl(
    context: &WorktreeContext<'_>,
    lanes: &[WorktreeInfo],
    policy: &WorktreeGcPolicy<'_>,
    cancel: &CancelToken,
) -> Result<WorktreeGcReport, WorktreeError> {
    validate_binding(context)?;
    let entries = context.facts.fresh_inventory(context, cancel)?;
    let primary = canonicalize(context.repo.root_path())?;
    let known_refs = RefLedger::observe_context(context)?
        .map(|ledger| ledger.list())
        .transpose()?
        .unwrap_or_default();
    let foreign_branch = retained_refs(context, None, None, &[], cancel)?
        .iter()
        .any(|target| {
            target.reference.as_str().starts_with("refs/heads/")
                && !entries
                    .iter()
                    .any(|entry| entry.branch.as_ref() == Some(&target.reference))
                && !known_refs.iter().any(|known| {
                    known.reference == target.reference.as_str()
                        && known.endpoint == RefEndpoint::Local
                        && known.origin == RefOrigin::Created
                })
        });
    let foreign_seen = foreign_branch
        || lanes
            .iter()
            .any(|lane| !lane.primary && matches!(lane.owner, Ok(None)));
    let first_foreign = policy.mode == CollectionMode::Worktrees
        && foreign_seen
        && policy.include_foreign
        && first_foreign_pass(context)?;
    let dry = policy.dry_run || first_foreign;
    let mut report = WorktreeGcReport {
        dry_run: dry,
        first_foreign_preview: first_foreign,
        ..Default::default()
    };
    let settings = provenant_config::ProjectGcSettings {
        regenerable: policy.regenerable.to_vec(),
        keep: policy.keep.to_vec(),
        ..Default::default()
    };
    let preview_walk = PreviewWalk::new(dry);
    let integration =
        policy
            .upstream
            .zip(policy.upstream_reference)
            .and_then(|(tip, reference)| {
                GitRefName::new(reference)
                    .ok()
                    .map(|reference| LandingTarget {
                        reference: Some(reference),
                        tip,
                    })
            });
    let patterns = policy.integration_patterns;
    let bases = policy
        .retirement_feeds
        .map(|feeds| feeds.integration_bases())
        .unwrap_or_default();
    for lane in lanes {
        if lane.primary || lane.path == primary {
            continue;
        }
        let class = if matches!(lane.owner, Ok(Some(_))) {
            "managed"
        } else {
            "foreign"
        };
        let mut row = CandidateReport {
            path: lane.path.clone(),
            class,
            action: "kept",
            tip: lane.head,
            detail: None,
        };
        let mut active_retirement: Option<RetirementRecord> = None;
        let result = (|| -> Result<(), WorktreeError> {
            if class == "foreign" && !policy.include_foreign {
                row.detail = Some("foreign cleanup disabled by repository configuration".into());
                return Ok(());
            }
            if policy
                .excluded_paths
                .iter()
                .any(|pattern| matches_pattern(&lane.path.to_string_lossy(), pattern))
            {
                row.detail = Some("excluded by gc.projects.keep_paths".into());
                return Ok(());
            }
            if lane.owner.is_err() {
                return Err(WorktreeError::UnknownWriter);
            }
            let entry = entries
                .iter()
                .find(|entry| entry.path == lane.path)
                .ok_or(WorktreeError::NotFound)?;
            if entry.locked || entry.prunable {
                return Err(WorktreeError::GitEntryHeld {
                    locked: entry.locked,
                });
            }
            // Never remove a symlink alias or a parent containing another checkout.
            let parent = lane.path.parent().ok_or(WorktreeError::InvalidRequest)?;
            let parent = Directory::open_anchor(parent)?;
            let name = lane.path.file_name().ok_or(WorktreeError::InvalidRequest)?;
            if parent.metadata_at(name, false)?.kind != FileKind::Directory {
                return Err(WorktreeError::InvalidRequest);
            }
            if entries
                .iter()
                .any(|other| other.path != lane.path && other.path.starts_with(&lane.path))
            {
                row.detail = Some("nested worktree retained".into());
                return Ok(());
            }
            if !(policy.no_live_lanes)(&lane.path) {
                report.removals.push(kept_landing_record(
                    context.binding.project_id(),
                    lane,
                    RemovalUncertainty::RunRecordsUnknown,
                ));
                return Err(WorktreeError::UnknownWriter);
            }
            if !dry && (policy.retirement_port.is_none() || policy.retirement_feeds.is_none()) {
                row.detail = Some("durable retirement owner unavailable".into());
                return Ok(());
            }
            if let Some(feeds) = policy.retirement_feeds {
                feeds
                    .custody(lane)
                    .map_err(|detail| WorktreeError::UnknownOccupancy {
                        path: lane.path.clone(),
                        detail,
                    })?;
            }
            let locked = lock_worktree(context, entry, cancel, dry)?;
            census_free(context, &lane.path)?;
            let tree_bytes = match allocated_tree(locked.verified.repo().root_dir(), 0) {
                Ok(bytes) => Some(bytes),
                Err(error) => {
                    report.byte_errors.push(RetentionIssue {
                        path: lane.path.clone(),
                        operation: "measure worktree allocated bytes".into(),
                        cause: error.to_string(),
                    });
                    None
                }
            };
            let effective = WorktreeGcPolicy {
                dry_run: dry,
                ..*policy
            };
            let ports = provenant_config::PortTable::read_project(&primary)
                .map_err(WorktreeError::PortConfiguration)?;
            let protected =
                output_protection(context, locked.verified.repo(), policy.keep, cancel)?;
            let output = if (policy.terminal)(&lane.path) {
                &mut report.output
            } else {
                &mut report.idle_output
            };
            let output_start = output.len();
            walk_output(
                locked.verified.repo().root_dir(),
                &lane.path,
                Path::new(""),
                &effective,
                &ports.worktree_layout.roots,
                &protected,
                output,
                0,
                &preview_walk,
            )?;

            let output_bytes = output[output_start..]
                .iter()
                .filter(|entry| entry.removed || entry.reason == "would_remove")
                .map(|entry| entry.bytes)
                .fold(0_u64, u64::saturating_add);
            let output_errors = output[output_start..]
                .iter()
                .filter(|entry| !entry.removed && entry.reason != "would_remove")
                .map(|entry| RetentionIssue {
                    path: entry.path.clone(),
                    operation: "remove declared output".into(),
                    cause: entry.reason.clone(),
                })
                .collect::<Vec<_>>();
            report.byte_errors.extend(output_errors);
            report.bytes = report.bytes.saturating_add(output_bytes);
            if policy.mode == CollectionMode::Outputs {
                row.detail = Some("output retention only".into());
                return Ok(());
            }
            let branch = lane
                .branch
                .as_deref()
                .map(GitRefName::new)
                .transpose()
                .map_err(|_| WorktreeError::InvalidOutput)?;
            let mut refs = retained_refs(
                context,
                branch.as_ref(),
                integration.as_ref(),
                &patterns,
                cancel,
            )?;
            for target in &mut refs {
                let full = target.reference.as_str();
                if bases.iter().any(|(name, base)| {
                    let names_base = full.strip_prefix("refs/heads/") == Some(name.as_str())
                        || (full.starts_with("refs/remotes/")
                            && full.ends_with(&format!("/{name}")));
                    names_base
                        && (*base == target.commit
                            || is_ancestor(context, *base, target.commit, cancel).unwrap_or(false))
                }) {
                    target.integration = true;
                }
            }
            let mut proof = removal_evidence(context, lane.head, &refs, cancel)?;
            if let (Some(evidence), Some(expected_target)) = (policy.proof, policy.upstream)
                && verify_retirement_landing_evidence(
                    context,
                    lane.head,
                    expected_target,
                    evidence,
                    cancel,
                )?
            {
                proof = LandingAssessment::Proven(evidence.clone());
            }
            let forge = match (policy.retirement_feeds, branch.as_ref()) {
                (Some(feeds), Some(branch)) => feeds.forge(branch),
                (_, None) => ForgeObservation::Absent,
                _ => ForgeObservation::Unknown("forge feed unavailable".into()),
            };
            if let ForgeObservation::Merged {
                pr,
                head,
                merge_commit,
            } = &forge
            {
                if *head == lane.head {
                    proof = LandingAssessment::Proven(LandedEvidence {
                        predicate: provenant_records::custody::LandingPredicate::Landed,
                        source: provenant_records::custody::LandingSource::ForgeMerged {
                            pr: pr.clone(),
                            head: *head,
                            merge_commit: *merge_commit,
                        },
                    });
                }
            }
            // Source output is disposable only by a declared class. Undeclared
            // ignored files (.env, databases) keep the tree, as do submodules.
            let action = match proof {
                LandingAssessment::Proven(evidence) => SelectedRetirement::Subsumed(evidence),
                LandingAssessment::Unknown(cause) => {
                    report.removals.push(kept_landing_record(
                        context.binding.project_id(),
                        lane,
                        cause,
                    ));
                    row.detail = Some(format!("uncertain Git evidence: {cause:?}"));
                    return Ok(());
                }
                LandingAssessment::NotSubsumed => {
                    let eligible_pr = branch.is_none()
                        || matches!(forge, ForgeObservation::Absent)
                        || matches!(forge, ForgeObservation::Closed { head } if head == lane.head);
                    if !eligible_pr {
                        report.removals.push(kept_landing_record(
                            context.binding.project_id(),
                            lane,
                            if matches!(forge, ForgeObservation::Unknown(_)) {
                                RemovalUncertainty::OfflineForge
                            } else {
                                RemovalUncertainty::UnprovenLanding
                            },
                        ));
                        row.detail = Some(format!("not subsumed; forge: {forge:?}"));
                        return Ok(());
                    }
                    let age = run_git(
                        context,
                        context.repo,
                        GitRead::CommitTime { tip: lane.head },
                        cancel,
                    )?;
                    if exit_code(&age)? != 0 {
                        return Err(WorktreeError::InvalidOutput);
                    }
                    let timestamp: i64 = std::str::from_utf8(trim_ascii(&age.stdout.bytes))
                        .map_err(|_| WorktreeError::InvalidOutput)?
                        .parse()
                        .map_err(|_| WorktreeError::InvalidOutput)?;
                    let newest =
                        preview_walk.newest(locked.verified.repo().root_dir(), 0, false)?;
                    let git_activity = latest_git_activity(
                        locked.verified.repo(),
                        branch.as_ref(),
                        i128::from(timestamp) * 1_000_000_000,
                    )?;
                    if git_activity >= policy.written_before_ns
                        || newest >= policy.written_before_ns
                    {
                        row.detail = Some("not subsumed; recent work retained".into());
                        return Ok(());
                    }
                    SelectedRetirement::Idle
                }
            };
            if dry {
                precheck_removal(
                    context,
                    locked.verified.repo(),
                    policy.regenerable,
                    policy.keep,
                    cancel,
                )?;
            }
            let decision = if dry {
                None
            } else {
                let feeds = policy
                    .retirement_feeds
                    .ok_or(WorktreeError::RegistrationUnconfirmed)?;
                Some(
                    action.decision(
                        feeds
                            .authority()
                            .map_err(|detail| WorktreeError::UnknownOccupancy {
                                path: lane.path.clone(),
                                detail,
                            })?,
                        branch.as_ref(),
                        lane.head,
                    ),
                )
            };
            let planned_cache = if dry {
                match crate::cache::preview_worktree_cargo_cache(context.home, &locked.verified) {
                    Ok(entry) => entry,
                    Err(error) => {
                        report.cache_errors.push(RetentionIssue {
                            path: lane.path.clone(),
                            operation: "measure private cache".into(),
                            cause: error.to_string(),
                        });
                        None
                    }
                }
            } else {
                None
            };
            drop(locked);
            let eligible = || -> Result<(), WorktreeError> {
                if let SelectedRetirement::Subsumed(evidence) = &action {
                    validate_removal_evidence(context, lane.head, evidence, &refs, cancel)?;
                    if let Some(branch) = branch.as_ref() {
                        validate_forge_head(
                            policy
                                .retirement_feeds
                                .ok_or(WorktreeError::RegistrationUnconfirmed)?,
                            branch,
                            evidence,
                        )?;
                    }
                }

                if !(policy.no_live_lanes)(&lane.path) {
                    return Err(WorktreeError::UnknownWriter);
                }
                if let Some(feeds) = policy.retirement_feeds {
                    feeds
                        .custody(lane)
                        .map_err(|detail| WorktreeError::UnknownOccupancy {
                            path: lane.path.clone(),
                            detail,
                        })?;
                }
                census_free(context, &lane.path)
            };
            eligible()?;
            if !dry {
                let port = policy
                    .retirement_port
                    .ok_or(WorktreeError::RegistrationUnconfirmed)?;
                let decision = decision
                    .clone()
                    .ok_or(WorktreeError::RegistrationUnconfirmed)?;
                let mut removed = remove_tree_when_before_delete(
                    context,
                    entry,
                    lane.head,
                    cancel,
                    &eligible,
                    Some(&settings),
                    Repository::Remove,
                    |verified, path, _archive| {
                        if canonicalize(path)? != canonicalize(&lane.path)? {
                            return Err(WorktreeError::IdentityChanged);
                        }
                        if let SelectedRetirement::Subsumed(evidence) = &action {
                            validate_removal_evidence(context, lane.head, evidence, &refs, cancel)?;
                            if let Some(branch) = branch.as_ref() {
                                validate_forge_head(
                                    policy
                                        .retirement_feeds
                                        .ok_or(WorktreeError::RegistrationUnconfirmed)?,
                                    branch,
                                    evidence,
                                )?;
                            }
                        }
                        let retirement = begin_worktree_retirement(
                            context, port, lane, verified, decision, cancel,
                        )?;
                        active_retirement = Some(retirement);
                        Ok(())
                    },
                )?;
                report.worktrees.removed.push(removed.info.clone());
                let mut retirement = active_retirement
                    .clone()
                    .ok_or(WorktreeError::RegistrationUnconfirmed)?;
                let completion = action.completion_evidence(lane.head);
                retirement =
                    finish_worktree_retirement(port, &retirement, &lane.path, completion.clone())?;
                active_retirement = Some(retirement.clone());
                let reference_completion = if action.removes_reference(branch.as_ref()) {
                    let branch = branch.as_ref().ok_or(WorktreeError::InvalidRequest)?;
                    let SelectedRetirement::Subsumed(evidence) = &action else {
                        return Err(WorktreeError::InvalidRequest);
                    };
                    let forge = if matches!(
                        &evidence.source,
                        LandingSource::ForgeMerged {
                            merge_commit: None,
                            ..
                        }
                    ) {
                        let feeds = policy
                            .retirement_feeds
                            .ok_or(WorktreeError::RegistrationUnconfirmed)?;
                        Some(|| feeds.forge(branch))
                    } else {
                        None
                    };
                    settle_branch(
                        context,
                        branch,
                        lane.head,
                        Disposition::Merged,
                        Some(evidence),
                        forge
                            .as_ref()
                            .map(|observe| observe as &dyn Fn() -> ForgeObservation),
                        cancel,
                        &mut report.worktrees,
                    )?
                    .map(|evidence| RemovalEvidence::Landing { evidence })
                } else {
                    None
                };
                if retirement.decision.removes_reference() {
                    retirement = finish_reference_retirement(
                        port,
                        &retirement,
                        reference_completion.is_some(),
                        reference_completion
                            .unwrap_or_else(|| retirement.decision.evidence().clone()),
                    )?;
                    active_retirement = Some(retirement.clone());
                }
                removed.repository = None;
                if let Some(cache) = removed.lane_cache.as_deref() {
                    match crate::cache::prune_worktree_cargo_cache(context.home, cache) {
                        Ok(bytes) => {
                            if bytes > 0 {
                                report.caches.push(crate::cache::CacheGcEntry {
                                    path: cache.to_owned(),
                                    bytes,
                                    reason: crate::cache::CacheGcReason::Orphaned,
                                });
                            }
                        }
                        Err(error) => report.cache_errors.push(RetentionIssue {
                            path: cache.to_owned(),
                            operation: "remove worktree private cache".into(),
                            cause: error.to_string(),
                        }),
                    }
                }
                report.removals.extend(retirement.removals());
            } else {
                if let Some(cache) = planned_cache {
                    report.caches.push(cache);
                }
                report.worktrees.removed.push(lane.clone());
                if action.removes_reference(branch.as_ref()) {
                    if let Some(branch) = &branch {
                        report
                            .worktrees
                            .pruned_refs
                            .push(branch.short_name().into());
                    }
                }
            }
            if let Some(bytes) = tree_bytes {
                report.bytes = report
                    .bytes
                    .saturating_add(bytes.saturating_sub(output_bytes));
            }

            row.action = match (&action, dry) {
                (SelectedRetirement::Idle, true) => "would_retire",
                (SelectedRetirement::Idle, false) => "retired",
                (_, true) => "would_remove",
                (_, false) => "removed",
            };
            match action {
                SelectedRetirement::Subsumed(evidence) => {
                    row.detail = Some(format!(
                        "{:?}; last fetched refs are used when forge is offline",
                        evidence.predicate
                    ));
                }
                SelectedRetirement::Idle => {
                    row.detail = Some("idle clean worktree retired; local branch retained".into());
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let (Some(port), Some(record)) = (policy.retirement_port, active_retirement.take()) {
                let failed = fail_worktree_retirement(port, &record, &lane.path, error.to_string());
                let mut failed = failed.unwrap_or(record);
                if failed.decision.removes_reference() {
                    let completion = failed.decision.evidence().clone();
                    failed = finish_reference_retirement(port, &failed, false, completion)
                        .unwrap_or(failed);
                }
                report.removals.extend(failed.removals());
            }
            if !report
                .removals
                .iter()
                .any(|r| r.subject.id == lane.path.to_string_lossy() && r.tip == lane.head)
            {
                let cause = match &error {
                    WorktreeError::IncompleteCensus { .. }
                    | WorktreeError::CensusUnknown { .. } => RemovalUncertainty::CensusUnknown,
                    WorktreeError::UnknownOccupancy { .. }
                    | WorktreeError::UnknownWriter
                    | WorktreeError::UnknownWriterAt { .. } => RemovalUncertainty::CustodyUnknown,
                    WorktreeError::LockBusy | WorktreeError::RegistryLockTimeout => {
                        RemovalUncertainty::Busy
                    }
                    WorktreeError::Dirty(_) | WorktreeError::IgnoredPaths(_) => {
                        RemovalUncertainty::DirtyWorktree
                    }
                    WorktreeError::GitEntryHeld { .. } => RemovalUncertainty::LockedOrUnavailable,
                    _ => RemovalUncertainty::ObservationFailed,
                };
                report.removals.push(kept_landing_record(
                    context.binding.project_id(),
                    lane,
                    cause,
                ));
            }
            row.detail = Some(error.to_string());
            if let Some(branch) = lane
                .branch
                .as_deref()
                .and_then(|name| GitRefName::new(name).ok())
            {
                retain_error(&mut report.worktrees, &branch, error);
            }
        }
        report.candidates.push(row);
    }
    if first_foreign && !policy.dry_run {
        acknowledge_foreign_preview(context)?;
    }
    Ok(report)
}

/// Removal needs a fresh positive census. Custody accepts an unobservable
/// process for ordinary removal; deleting a tree automatically does not.
fn census_free(context: &WorktreeContext<'_>, path: &Path) -> Result<(), WorktreeError> {
    context.workers.invalidate();
    match census_refusal_detail(context, path) {
        Some(detail) => Err(WorktreeError::IncompleteCensus { detail }),
        None => Ok(()),
    }
}

fn census_refusal_detail(context: &WorktreeContext<'_>, path: &Path) -> Option<String> {
    let (states, details) = context
        .workers
        .observe_roots_with_details(std::slice::from_ref(&path.to_path_buf()));
    match states.get(path) {
        Some(Occupancy::Available) => None,
        Some(Occupancy::Unknown { evidence, .. }) => Some(
            details
                .get(path)
                .cloned()
                .unwrap_or_else(|| evidence.clone()),
        ),
        Some(_) => Some(
            details
                .get(path)
                .cloned()
                .unwrap_or_else(|| "worker census did not prove the tree free".into()),
        ),
        None => Some("worker census did not return the requested tree".into()),
    }
}

fn retain_error(outcome: &mut RemoveOutcome, branch: &GitRefName, error: WorktreeError) {
    let detail = match &error {
        WorktreeError::IncompleteCensus { detail } => Some(detail.clone()),
        _ => None,
    };
    if let Some(detail) = detail {
        retain_with_detail(outcome, branch, retain_reason(&error), detail);
    } else {
        retain(outcome, branch, retain_reason(&error));
    }
}

fn retain_with_detail(
    outcome: &mut RemoveOutcome,
    branch: &GitRefName,
    reason: RetainReason,
    detail: String,
) {
    retain(outcome, branch, reason);
    if let Some(retained) = outcome.retained.last_mut() {
        retained.detail = Some(detail);
    }
}

/// Membership checks cost path depth, rather than a scan of the whole index
/// for every filesystem entry. Ancestors protect directories containing files.
struct ProtectedPaths {
    paths: HashSet<PathBuf>,
    ancestors: HashSet<PathBuf>,
    ignored: HashSet<PathBuf>,
}

impl ProtectedPaths {
    fn new(keep: &[String]) -> Self {
        let mut protected = Self {
            paths: HashSet::new(),
            ancestors: HashSet::new(),
            ignored: HashSet::new(),
        };
        for path in keep {
            protected.insert(Path::new(path));
        }
        protected
    }

    fn insert(&mut self, path: &Path) {
        self.paths.insert(path.to_owned());
        self.ancestors.extend(path.ancestors().map(Path::to_owned));
    }

    fn covers(&self, path: &Path) -> bool {
        path.ancestors()
            .any(|ancestor| self.paths.contains(ancestor))
    }
}

fn clean_after_preview(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    root: &Path,
    output: &[OutputEntry],
    cancel: &CancelToken,
    preview_walk: &PreviewWalk,
) -> Result<bool, WorktreeError> {
    preview_walk.step()?;
    let status = run_git(context, repo, GitRead::StatusIncludingIgnored, cancel)?;
    if exit_code(&status)? != 0 {
        return Err(WorktreeError::UnknownWriter);
    }
    let records =
        parse_porcelain_v2_z(&status.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    let planned: HashSet<_> = output
        .iter()
        .filter(|entry| entry.reason == "would_remove")
        .map(|entry| entry.path.clone())
        .collect();
    for record in records {
        if record.kind != StatusKind::Ignored {
            return Ok(false);
        }
        let path =
            Path::new(std::str::from_utf8(&record.path).map_err(|_| WorktreeError::InvalidOutput)?);
        if !path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(WorktreeError::InvalidOutput);
        }
        let absolute = root.join(path);
        if absolute
            .ancestors()
            .any(|ancestor| planned.contains(ancestor))
        {
            continue;
        }
        // Git may collapse an ignored parent containing several outputs.
        // Every remaining leaf must belong to an eligible output directory.
        let mut components = path.components();
        let first = components.next().ok_or(WorktreeError::InvalidOutput)?;
        if repo.root_dir().metadata_at(first.as_os_str(), false)?.kind != FileKind::Directory {
            return Ok(false);
        }
        let mut directory = repo.root_dir().open_verified_directory(first.as_os_str())?;
        for component in components {
            if directory.metadata_at(component.as_os_str(), false)?.kind != FileKind::Directory {
                return Ok(false);
            }
            directory = directory.open_verified_directory(component.as_os_str())?;
        }
        if !preview_directory_removed(&directory, &absolute, &planned, 0, preview_walk)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn preview_directory_removed(
    directory: &Directory,
    path: &Path,
    planned: &HashSet<PathBuf>,
    depth: usize,
    preview_walk: &PreviewWalk,
) -> Result<bool, FsError> {
    preview_walk.step()?;
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    if contains_repository(directory)? {
        return Ok(false);
    }
    for name in entry_names(
        directory,
        if preview_walk.enabled {
            20_000
        } else {
            100_000
        },
    )? {
        preview_walk.step()?;
        let child = path.join(&name);
        if planned.contains(&child) {
            continue;
        }
        if directory.metadata_at(&name, false)?.kind != FileKind::Directory {
            return Ok(false);
        }
        if !preview_directory_removed(
            &directory.open_verified_directory(&name)?,
            &child,
            planned,
            depth + 1,
            preview_walk,
        )? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn walk_output(
    directory: &Directory,
    root: &Path,
    relative: &Path,
    policy: &WorktreeGcPolicy<'_>,
    layout_roots: &[PathBuf],
    protected: &ProtectedPaths,
    report: &mut Vec<OutputEntry>,
    depth: usize,
    preview_walk: &PreviewWalk,
) -> Result<(), FsError> {
    preview_walk.step()?;
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    for name in entry_names(
        directory,
        if preview_walk.enabled {
            20_000
        } else {
            100_000
        },
    )? {
        preview_walk.step()?;
        if name == ".git" {
            continue;
        }
        let path = relative.join(&name);
        if layout_roots
            .iter()
            .any(|root| path.as_path() == root.as_path())
            || protected.covers(&path)
        {
            continue;
        }
        let mut prune = || -> Result<Option<u64>, FsError> {
            let meta = directory.metadata_at(&name, false)?;
            if meta.kind != FileKind::Directory {
                return Ok(None);
            }
            let child = directory.open_verified_directory(&name)?;
            if contains_repository(&child)? {
                return Ok(None);
            }
            let contains_layout_root = layout_roots.iter().any(|root| root.starts_with(&path));
            if !contains_layout_root
                && (policy
                    .regenerable
                    .iter()
                    .any(|configured| name == OsStr::new(configured))
                    || is_default_regenerable(&path))
                && !protected.ancestors.contains(&path)
                && path
                    .ancestors()
                    .any(|ancestor| protected.ignored.contains(ancestor))
            {
                let bytes = allocated_tree(&child, 0)?;
                prune_output(directory, &name, policy, preview_walk)?;
                Ok(Some(bytes))
            } else {
                walk_output(
                    &child,
                    root,
                    &path,
                    policy,
                    layout_roots,
                    protected,
                    report,
                    depth + 1,
                    preview_walk,
                )?;
                Ok(None)
            }
        };
        match prune() {
            Ok(Some(bytes)) => report.push(OutputEntry {
                bytes,
                path: root.join(path),
                removed: !policy.dry_run,
                reason: if policy.dry_run {
                    "would_remove"
                } else {
                    "removed"
                }
                .into(),
            }),
            Ok(None) => {}
            Err(error) => report.push(OutputEntry {
                bytes: 0,
                path: root.join(path),
                removed: false,
                reason: error.to_string(),
            }),
        }
    }
    Ok(())
}

/// Provenant's own lane scratch is regenerable without a project declaration.
/// The walker still prunes it only when Git reports it ignored, and only with
/// the same writer custody and census proof as declared output.
fn is_default_regenerable(path: &Path) -> bool {
    path == Path::new(crate::scratch::PROJECT_SCRATCH)
}

/// A .git file or directory marks a protected repository boundary.
fn contains_repository(directory: &Directory) -> Result<bool, FsError> {
    match directory.metadata_at(OsStr::new(".git"), false) {
        Ok(_) => Ok(true),
        Err(error) if error.is_not_found() => Ok(false),
        Err(error) => Err(error),
    }
}

// Real time bounds the idle proof; explicit removal needs only repository protection.
#[allow(clippy::disallowed_methods)]
fn check_output_before(directory: &Directory, before: i128) -> Result<(), FsError> {
    check_output_bounded(directory, before, &mut 1_000_000, std::time::Instant::now())
}

fn check_output_bounded(
    directory: &Directory,
    before: i128,
    remaining: &mut usize,
    started: std::time::Instant,
) -> Result<(), FsError> {
    if before == i128::MAX {
        return protect_repositories(directory, 0);
    }
    if newest_bounded(directory, 0, remaining, started)? >= before {
        return Err(FsError::RaceRefused {
            name: "recent output".into(),
        });
    }
    Ok(())
}

fn protect_repositories(directory: &Directory, depth: usize) -> Result<(), FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    if contains_repository(directory)? {
        return Err(FsError::RaceRefused {
            name: ".git".into(),
        });
    }
    for name in entry_names(directory, 100_000)? {
        if directory.metadata_at(&name, false)?.kind == FileKind::Directory {
            protect_repositories(&directory.open_verified_directory(&name)?, depth + 1)?;
        }
    }
    Ok(())
}

fn newest_bounded(
    directory: &Directory,
    depth: usize,
    remaining: &mut usize,
    started: std::time::Instant,
) -> Result<i128, FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    if contains_repository(directory)? {
        return Err(FsError::RaceRefused {
            name: ".git".into(),
        });
    }
    let mut time = directory.metadata()?.modified_ns;
    for name in entry_names(directory, 100_000)? {
        if *remaining == 0 || started.elapsed() >= std::time::Duration::from_secs(5) {
            return Err(FsError::EntryLimit { limit: 1_000_000 });
        }
        *remaining -= 1;
        let meta = directory.metadata_at(&name, false)?;
        time = time.max(meta.modified_ns);
        if meta.kind == FileKind::Directory {
            time = time.max(newest_bounded(
                &directory.open_verified_directory(&name)?,
                depth + 1,
                remaining,
                started,
            )?);
        }
    }
    Ok(time)
}

fn prune_output(
    parent: &Directory,
    name: &OsStr,
    policy: &WorktreeGcPolicy<'_>,
    preview_walk: &PreviewWalk,
) -> Result<(), FsError> {
    let directory = parent.open_verified_directory(name)?;
    let identity = directory.metadata()?;
    if policy.dry_run {
        let newest = preview_walk.newest(&directory, 0, true)?;
        if newest >= policy.written_before_ns {
            return Err(FsError::RaceRefused {
                name: "recent output".into(),
            });
        }
        return Ok(());
    }
    check_output_before(&directory, policy.written_before_ns)?;
    let staging_name = temporary_name()?;
    parent.mkdir(&staging_name, 0o700)?;
    let staging = parent.open_verified_directory(&staging_name)?;
    let payload = PayloadTree::claim(parent, &staging_name, &staging.metadata()?)?;
    // A private parent grants payload disposal without changing the original
    // directory's permissions or following links in the output tree.
    if let Err(error) = parent.rename_noreplace(name, &staging, OsStr::new("output")) {
        parent.rmdir(&staging_name)?;
        return Err(error);
    }
    let removed = || -> Result<(), FsError> {
        let detached = payload.child(OsStr::new("output"))?;
        if !identity.same_identity(&detached.metadata()?) {
            return Err(FsError::RaceRefused { name: name.into() });
        }
        check_output_before(detached.directory(), policy.written_before_ns)?;
        remove_output(&detached, 0)?;
        payload.rmdir(OsStr::new("output"))
    };
    if let Err(error) = removed() {
        staging.rename_noreplace(OsStr::new("output"), parent, name)?;
        parent.rmdir(&staging_name)?;
        return Err(error);
    }
    parent.rmdir(&staging_name)?;
    parent.sync()
}

pub(super) fn dispose_archive_tree(tree: &PayloadTree) -> Result<(), FsError> {
    tree.make_writable()?;
    for name in tree.entries()? {
        let name = name?;
        if tree.metadata_at(&name)?.kind == FileKind::Directory {
            tree.directory().make_owner_accessible_at(&name)?;
            dispose_archive_tree(&tree.child(&name)?)?;
            tree.rmdir(&name)?;
        } else {
            tree.unlink(&name)?;
        }
    }
    Ok(())
}

fn remove_output(tree: &PayloadTree, depth: usize) -> Result<(), FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    if contains_repository(tree.directory())? {
        return Err(FsError::RaceRefused {
            name: ".git".into(),
        });
    }
    tree.make_writable()?;
    for name in entry_names(tree.directory(), 100_000)? {
        if tree.metadata_at(&name)?.kind == FileKind::Directory {
            remove_output(&tree.child(&name)?, depth + 1)?;
            tree.rmdir(&name)?;
        } else {
            tree.unlink(&name)?;
        }
    }
    Ok(())
}

/// Prove all ignored paths are disposable before removing any output.
/// The caller holds writer custody.
pub(super) fn prepare_removal(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    regenerable: &[String],
    keep: &[String],
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    prepare_removal_before(context, repo, regenerable, keep, false, cancel)
}

/// Preview exactly the same ignored-output eligibility as actual removal.
pub(super) fn precheck_removal(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    regenerable: &[String],
    keep: &[String],
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    prepare_removal_before(context, repo, regenerable, keep, true, cancel)
}

fn prepare_removal_before(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    regenerable: &[String],
    keep: &[String],
    preview_only: bool,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    let ports = provenant_config::PortTable::read_project(context.repo.root_path())
        .map_err(WorktreeError::PortConfiguration)?;
    let layout_roots = ports.worktree_layout.roots;
    let protected = output_protection(context, repo, keep, cancel)?;
    let mut policy = WorktreeGcPolicy {
        upstream: None,
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable,
        keep,
        written_before_ns: i128::MAX,
        dry_run: true,
        include_foreign: false,
        retirement_port: None,
        retirement_feeds: None,
        mode: CollectionMode::Outputs,
        proof: None,
        upstream_reference: None,
        integration_patterns: &[],
        excluded_paths: &[],
    };
    let mut output = Vec::new();
    let preview_walk = PreviewWalk::new(preview_only);
    walk_output(
        repo.root_dir(),
        repo.root_path(),
        Path::new(""),
        &policy,
        &layout_roots,
        &protected,
        &mut output,
        0,
        &preview_walk,
    )?;
    if !clean_after_preview(
        context,
        repo,
        repo.root_path(),
        &output,
        cancel,
        &preview_walk,
    )? {
        let status = run_git(context, repo, GitRead::StatusIncludingIgnored, cancel)?;
        let planned: Vec<_> = output
            .iter()
            .filter(|entry| entry.reason == "would_remove")
            .collect();
        let paths = parse_porcelain_v2_z(&status.stdout.bytes)
            .map_err(|_| WorktreeError::InvalidOutput)?
            .into_iter()
            .filter(|record| {
                !planned.iter().any(|entry| {
                    repo.root_path()
                        .join(OsStr::from_bytes(&record.path))
                        .starts_with(&entry.path)
                })
            })
            .take(5)
            .map(|record| String::from_utf8_lossy(&record.path).into_owned())
            .collect::<Vec<_>>();
        return Err(WorktreeError::IgnoredPaths(paths.join(", ")));
    }
    if preview_only {
        return Ok(());
    }
    policy.dry_run = false;
    output.clear();
    walk_output(
        repo.root_dir(),
        repo.root_path(),
        Path::new(""),
        &policy,
        &layout_roots,
        &protected,
        &mut output,
        0,
        &preview_walk,
    )?;
    Ok(())
}

fn output_protection(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    keep: &[String],
    cancel: &CancelToken,
) -> Result<ProtectedPaths, WorktreeError> {
    let mut protected = ProtectedPaths::new(keep);
    let index = run_git(
        context,
        repo,
        GitRead::IndexEntries { paths: Vec::new() },
        cancel,
    )?;
    if exit_code(&index)? != 0 {
        return Err(WorktreeError::UnknownWriter);
    }
    for entry in
        parse_index_entries(&index.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?
    {
        let entry = entry.map_err(|_| WorktreeError::InvalidOutput)?;
        protected.insert(Path::new(
            std::str::from_utf8(entry.path).map_err(|_| WorktreeError::InvalidOutput)?,
        ));
    }
    protect_nonignored(context, repo, &mut protected, cancel)?;
    Ok(protected)
}

/// An untracked directory named target is not a declaration of disposability.
fn protect_nonignored(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    protected: &mut ProtectedPaths,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    let status = run_git(context, repo, GitRead::StatusIncludingIgnored, cancel)?;
    if exit_code(&status)? != 0 {
        return Err(WorktreeError::UnknownWriter);
    }
    for record in
        parse_porcelain_v2_z(&status.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?
    {
        let path =
            Path::new(std::str::from_utf8(&record.path).map_err(|_| WorktreeError::InvalidOutput)?);
        if !path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(WorktreeError::InvalidOutput);
        }
        if record.kind == StatusKind::Ignored {
            protected.ignored.insert(path.to_owned());
        } else {
            protected.insert(path);
        }
    }
    Ok(())
}
