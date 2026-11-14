//! The one landing owner (contract C4).
//!
//! Every publisher submits a [`LandRequest`] here: `provenant land`, a graph
//! land node and, through it, graph delivery. The owner pins each member's
//! head and base, keys the request by them, and drives the candidate driver
//! (CI wait, rebase onto the target, gate, merge, local clean-up). Its record
//! under `landing/requests/<request-key>/<id>.json` holds per-member outcomes, so a
//! resubmission is idempotent (a landed member is never landed again) and a
//! restart resumes from the record and the driver's publication markers.
//!
//! The request already carries what later slices need: a queue (#3127) reads
//! the records, a stack (#3133) submits its members in order, and a batch on a
//! base other than the default (#3191) pins that base. This slice lands one
//! target per request and refuses a member pinned to another base.
use super::{
    LandingError,
    approval::Approvals,
    driver,
    model::{GateDefinition, Outcome, Pin},
    policy::LandPolicy,
    ports::{
        AlarmSink, Forge, GateRunner, PullRequestState, Repo, ReviewRecordSource, ScopedReviews,
    },
    receipt,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use provenant_records::landing::{
    Authority, LandMember, LandRequest, LandingId, LandingRecord, MemberOutcome, ReviewProof,
};
use provenant_records::landing::{
    CandidateKind, CleanupDebt, CleanupDebtItem, Dependencies, LandingChanged, LandingContext,
    MemberProgress, Observation, OutboxEntry, PublicationGuarantee, Submission,
};

/// The landing port every publisher uses.
pub trait LandingOwner {
    /// Pin, key and advance a landing as far as it goes in one bounded call.
    fn submit(&mut self, request: LandRequest) -> Result<LandingId, LandingError>;
    /// The durable record of a submitted landing.
    fn record(&self, id: &LandingId) -> Result<Option<LandingRecord>, LandingError>;
    /// Observation execution follows in 2.1; its correlation contract is frozen now.
    fn observe(&mut self, _event: Observation) -> Result<LandingRecord, LandingError> {
        Err(LandingError::Unimplemented(
            "landing observation executes in 2.1",
        ))
    }
}

/// Effects and fresh policy enter through the adapter. Resumption restores the
/// saved policy and gate; explicit selection changes create a linked generation.
pub struct Owner<'a> {
    pub forge: &'a mut dyn Forge,
    pub repo: &'a mut dyn Repo,
    pub runner: &'a mut dyn GateRunner,
    pub alarms: &'a mut dyn AlarmSink,
    pub state: &'a Path,
    pub policy: &'a LandPolicy,
    pub gate: &'a GateDefinition,
    pub reviews: &'a dyn ReviewRecordSource,
    pub review_scope: provenant_records::landing::ReviewScope,
    /// Shadow runs never publish and keep no landing record.
    pub shadow: bool,
    /// The last driver pass, for an adapter that renders it.
    pub last: Option<(driver::Report, Option<LandingError>)>,
}

fn request_dir(state: &Path, key: provenant_core::Digest) -> PathBuf {
    state.join("landing").join("requests").join(key.to_string())
}

fn record_path(state: &Path, id: &LandingId) -> PathBuf {
    request_dir(state, id.request).join(format!("{id}.json"))
}

fn decode_record(bytes: &[u8]) -> Result<LandingRecord, LandingError> {
    let record: LandingRecord = serde_json::from_slice(bytes)
        .map_err(|error| LandingError::Failed(format!("read landing record: {error}")))?;
    validate_request(&record.request)?;
    let saved_policy = LandPolicy::from_plan(&record.resolved_plan)
        .map_err(|error| LandingError::Failed(format!("invalid saved policy: {error}")))?;
    LandPolicy::review_scope(&record.resolved_plan)
        .map_err(|error| LandingError::Failed(format!("invalid saved review scope: {error}")))?;
    let digest = record
        .resolved_plan
        .computed_digest()
        .map_err(|error| LandingError::Failed(format!("verify saved plan: {error}")))?;
    let settings = saved_policy
        .settings()
        .map_err(|error| LandingError::Failed(format!("verify saved settings: {error}")))?;
    let mut gate = super::gate_definition(&settings.gate)
        .map_err(|error| LandingError::Failed(format!("verify saved gate: {error}")))?;
    // Patience is a per-pass budget; the checked criteria and digest stay frozen.
    gate.timeout_seconds = record.gate_definition.timeout_seconds;
    if digest != record.resolved_plan.digest || gate != record.gate_definition {
        return Err(LandingError::Failed(
            "saved landing plan or gate seal mismatch".into(),
        ));
    }
    let identity = LandingId::of(
        &record.repository,
        &record.target,
        &record.request,
        &record.resolved_plan.digest,
    )
    .map_err(|error| LandingError::Failed(format!("key landing record: {error}")))?;
    if record.schema_version != 2
        || identity != record.id
        || record.generation != record.request.generation
        || record.request.target.as_deref() != Some(record.target.as_str())
        || record
            .request
            .strategy
            .is_some_and(|strategy| strategy != record.resolved_plan.strategy)
        || record
            .request
            .members
            .iter()
            .any(|member| member.head.is_none() || member.base.is_none())
    {
        return Err(LandingError::Failed(format!(
            "inconsistent landing record {}",
            record.id
        )));
    }
    Ok(record)
}

pub fn read_record(state: &Path, id: &LandingId) -> Result<Option<LandingRecord>, LandingError> {
    match std::fs::read(record_path(state, id)) {
        Ok(bytes) => {
            let record = decode_record(&bytes)?;
            if &record.id != id {
                return Err(LandingError::Failed(
                    "landing record identity mismatch".into(),
                ));
            }
            Ok(Some(record))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(LandingError::Failed(format!(
            "read landing record {id}: {error}"
        ))),
    }
}

/// The records are the registry. Only generations of this pinned input are read;
/// no mutable head pointer or second index can disagree with them after a crash.
fn latest_record(
    state: &Path,
    key: provenant_core::Digest,
    parent: Option<&LandingId>,
) -> Result<Option<LandingRecord>, LandingError> {
    let entries = match std::fs::read_dir(request_dir(state, key)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(LandingError::Failed(format!(
                "read landing generations: {error}"
            )));
        }
    };
    let mut latest: Option<LandingRecord> = None;
    let mut ambiguous = false;
    for entry in entries {
        let path = entry
            .map_err(|error| LandingError::Failed(format!("read landing generation: {error}")))?
            .path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let bytes = std::fs::read(path)
            .map_err(|error| LandingError::Failed(format!("read landing generation: {error}")))?;
        let record = decode_record(&bytes)?;
        if record.id.request != key {
            return Err(LandingError::Failed("landing request key mismatch".into()));
        }
        if parent.is_some() && record.request.parent.as_ref() != parent {
            continue;
        }
        match &latest {
            Some(previous)
                if previous.generation == record.generation && previous.id != record.id =>
            {
                ambiguous = true;
            }
            Some(previous) if previous.generation >= record.generation => {}
            _ => {
                latest = Some(record);
                ambiguous = false;
            }
        }
    }
    if ambiguous {
        return Err(LandingError::Failed(
            "ambiguous landing generation; select an explicit parent".into(),
        ));
    }
    Ok(latest)
}

fn write_record(state: &Path, record: &LandingRecord) -> Result<(), LandingError> {
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|error| LandingError::Failed(format!("encode landing record: {error}")))?;
    receipt::atomic_write(&record_path(state, &record.id), &bytes)
}

fn settlement_record(
    state: &Path,
    record: LandingRecord,
    shadow: bool,
) -> Result<(Option<provenant_os::fs::LockGuard>, LandingRecord), LandingError> {
    if shadow {
        return Ok((None, record));
    }
    let mutation = receipt::lock(state)?;
    let current = read_record(state, &record.id)?.ok_or_else(|| {
        LandingError::Failed("landing record disappeared before settlement".into())
    })?;
    Ok((Some(mutation), current))
}

fn record_interaction(record: &mut LandingRecord, request: &LandRequest, expected_revision: u64) {
    if record.revision != expected_revision {
        return;
    }
    record.request.authority = request.authority;
    record.request.policy = request.policy.clone();
    for member in &mut record.request.members {
        if let Some(fresh) = request.members.iter().find(|fresh| fresh.pr == member.pr) {
            member.review_proof = fresh.review_proof.clone();
        }
    }
}

/// Older negative or pending observations cannot replace a newer decision.
/// A confirmed publication remains valid evidence even if its result arrived late.
fn apply_pass_outcomes(
    record: &mut LandingRecord,
    expected_revision: u64,
    outcomes: BTreeMap<u64, MemberOutcome>,
) -> bool {
    let stale = record.revision != expected_revision;
    for (pr, outcome) in outcomes {
        if (!stale || outcome.landed())
            && !record.outcomes.get(&pr).is_some_and(MemberOutcome::landed)
        {
            record.outcomes.insert(pr, outcome);
        }
    }
    stale
}

fn selection_changed(request: &LandRequest, record: &LandingRecord) -> bool {
    request
        .strategy
        .is_some_and(|strategy| strategy != record.resolved_plan.strategy)
        || request.compose.apply_to(record.resolved_plan.plan) != record.resolved_plan.plan
        || request
            .policy
            .tier
            .as_ref()
            .is_some_and(|tier| Some(tier) != record.request.policy.tier.as_ref())
}

/// Called only while holding the publication lock. Allocation and settlement
/// share that lock, so identity, generation and revision remain fenced through dispatch.
pub(super) fn fence(state: &Path, context: &LandingContext) -> Result<(), LandingError> {
    let current = latest_record(state, context.id.request, None)?
        .ok_or_else(|| LandingError::Failed("landing publication record is missing".into()))?;
    if current.id != context.id
        || current.generation != context.generation
        || current.revision != context.expected_revision
    {
        return Err(LandingError::Pending(
            "landing generation or snapshot was superseded; resubmit".into(),
        ));
    }
    Ok(())
}

/// Reuse the existing nonblocking publication exclusion only for local allocation.
/// Capability/forge reads happen before this function, and execution after it.
fn allocate_generation(
    state: &Path,
    mut record: LandingRecord,
    fresh: bool,
    request: &LandRequest,
) -> Result<LandingRecord, LandingError> {
    let _mutation = receipt::lock(state)?;
    if let Some(latest) = latest_record(state, record.id.request, None)? {
        if latest.id == record.id {
            return Ok(latest);
        }
        if !fresh {
            if selection_changed(request, &latest) {
                return Err(LandingError::Busy);
            }
            return Ok(latest);
        }
        if latest.generation >= record.generation {
            record.request.parent = Some(latest.id);
            record.generation = latest
                .generation
                .checked_add(1)
                .ok_or_else(|| LandingError::Failed("landing generation overflow".into()))?;
            record.request.generation = record.generation;
            record.id = LandingId::of(
                &record.repository,
                &record.target,
                &record.request,
                &record.resolved_plan.digest,
            )
            .map_err(|error| LandingError::Failed(format!("key allocated generation: {error}")))?;
        }
    }
    write_record(state, &record)?;
    Ok(record)
}

/// A driver failure that a later submission can get past without a change.
fn transient(error: &LandingError) -> bool {
    matches!(
        error,
        LandingError::Pending(_)
            | LandingError::Busy
            | LandingError::RateLimited(_)
            | LandingError::CiUnavailable(_)
            | LandingError::Contention
            | LandingError::TargetMoved
    )
}

/// Each member's outcome from one driver pass.
pub(super) fn member_outcomes(
    members: &[LandMember],
    report: &driver::Report,
    failure: Option<&LandingError>,
) -> BTreeMap<u64, MemberOutcome> {
    let cause = failure.map(ToString::to_string);
    let cause = || cause.clone().unwrap_or_else(|| "not reached".into());
    members
        .iter()
        .map(|member| {
            let reason = report.exclusion_reasons.get(&member.pr).cloned();
            let cleanup_unsettled = report.local_steps.get(&member.pr).is_some_and(|local| {
                local["cleanup_pending"] == true || local["error"].is_string()
            });
            let outcome = match report.outcomes.get(&member.pr) {
                Some(Outcome::Landed) if cleanup_unsettled => MemberOutcome::Pending {
                    reason: "local cleanup is pending; resubmit to retry it".into(),
                },
                Some(Outcome::Landed) => MemberOutcome::Landed {
                    commit: member.head.as_ref().and_then(|head| {
                        report
                            .receipts
                            .iter()
                            .find_map(|receipt| receipt.approved_to_landed.get(head).cloned())
                    }),
                },
                Some(Outcome::PendingUnknown) => MemberOutcome::Unknown { reason: cause() },
                Some(Outcome::Pending) => MemberOutcome::Pending { reason: cause() },
                Some(Outcome::Red) => MemberOutcome::Refused {
                    reason: report
                        .warnings
                        .iter()
                        .find(|warning| warning.contains(&format!("#{}:", member.pr)))
                        .cloned()
                        .unwrap_or_else(|| "failed gate checks".into()),
                },
                Some(Outcome::Conflicted) => MemberOutcome::Refused {
                    reason: format!("pull request #{} conflicts with the target", member.pr),
                },
                Some(Outcome::Excluded) | None => match (reason, failure) {
                    (Some(reason), _) => MemberOutcome::Refused { reason },
                    (None, Some(error)) if transient(error) => {
                        MemberOutcome::Pending { reason: cause() }
                    }
                    (None, Some(_)) => MemberOutcome::Refused { reason: cause() },
                    (None, None) => MemberOutcome::Pending { reason: cause() },
                },
            };
            (member.pr, outcome)
        })
        .collect()
}

impl Owner<'_> {
    /// Fill each member's unset head and base from the forge.
    fn pin(&mut self, members: &mut [LandMember]) -> Result<(), LandingError> {
        for member in members.iter_mut() {
            if member.head.is_none() || member.base.is_none() {
                let info = self.forge.pull_request(member.pr)?;
                member.head.get_or_insert(info.head);
                member.base.get_or_insert(info.base_branch);
            }
        }
        Ok(())
    }

    /// Record whether the pinned heads were merged by someone else.
    fn observe(
        &mut self,
        members: &[LandMember],
    ) -> Result<BTreeMap<u64, MemberOutcome>, LandingError> {
        let mut outcomes = BTreeMap::new();
        for member in members {
            let info = self.forge.pull_request(member.pr)?;
            let head = member.head.as_deref().unwrap_or_default();
            let outcome = if info.head != head {
                MemberOutcome::Refused {
                    reason: format!(
                        "HEAD_MOVED: pull request #{} head moved to {}",
                        member.pr, info.head
                    ),
                }
            } else if member
                .base
                .as_deref()
                .is_some_and(|base| base != info.base_branch)
            {
                MemberOutcome::Refused {
                    reason: format!(
                        "BASE_CHANGED: pull request #{} now targets {}",
                        member.pr, info.base_branch
                    ),
                }
            } else {
                match info.state {
                    PullRequestState::Merged => MemberOutcome::Landed {
                        commit: info.merged_commit,
                    },
                    PullRequestState::Open => MemberOutcome::Pending {
                        reason: format!(
                            "pull request #{} awaits the chair; next: provenant land {} --head {head}",
                            member.pr, member.pr
                        ),
                    },
                    PullRequestState::Closed => MemberOutcome::Refused {
                        reason: format!(
                            "PR_CLOSED: pull request #{} is closed without merging",
                            member.pr
                        ),
                    },
                }
            };
            outcomes.insert(member.pr, outcome);
        }
        Ok(outcomes)
    }

    /// One driver pass over the members not yet landed.
    fn drive(
        &mut self,
        members: &[LandMember],
        target: &str,
        context: LandingContext,
        mut policy: LandPolicy,
        gate: &GateDefinition,
    ) -> Result<BTreeMap<u64, MemberOutcome>, LandingError> {
        let mut settings = policy
            .settings()
            .map_err(|error| LandingError::Failed(format!("restore landing settings: {error}")))?;
        if let Some(poll) = context.request.policy.poll_seconds {
            settings.poll_seconds = poll;
        }
        let mut gate = (*gate).clone();
        gate.timeout_seconds = context
            .request
            .policy
            .patience_seconds
            .unwrap_or(settings.gate.timeout_seconds);
        let scope = LandPolicy::review_scope(&context.plan)
            .map_err(|error| LandingError::Failed(format!("restore review scope: {error}")))?;
        let exempt =
            provenant_confinement::review_exempt::ReviewExempt::new(&scope.exempt_patterns);
        let reviews = ScopedReviews {
            source: self.reviews,
            scope: &scope,
        };
        if context.plan.plan.candidate == CandidateKind::Single {
            settings.batch_size = 1;
        }
        policy.landing = Some(context);
        let numbers: Vec<u64> = members.iter().map(|member| member.pr).collect();
        let pins: BTreeMap<u64, Pin> = members
            .iter()
            .map(|member| {
                (
                    member.pr,
                    Pin {
                        head: member.head.clone(),
                        base: member.base.clone(),
                    },
                )
            })
            .collect();
        let acceptances: BTreeMap<u64, String> = members
            .iter()
            .filter_map(|member| match &member.review_proof {
                ReviewProof::Policy => None,
                ReviewProof::Accepted { reason } => Some((member.pr, reason.clone())),
            })
            .collect();
        let result = driver::run(
            &mut *self.forge,
            &mut *self.repo,
            &mut *self.runner,
            &mut *self.alarms,
            driver::Request {
                numbers: &numbers,
                target,
                settings: &settings,
                policy: &policy,
                gate: &gate,
                shadow: self.shadow,
                state: self.state,
                approvals: Approvals::new(&settings.approval_sources, &reviews)
                    .governed(settings.approval_tier, &exempt, None)
                    .with_acceptances(&acceptances),
                pins: &pins,
            },
        );
        let (report, failure) = match result {
            Ok(report) => (report, None),
            Err(LandingError::RunFailed { cause, report }) => (*report, Some(*cause)),
            Err(error) => (driver::Report::default(), Some(error)),
        };
        let outcomes = member_outcomes(members, &report, failure.as_ref());
        self.last = Some((report, failure));
        Ok(outcomes)
    }
}

/// A request graph must name unique members and a DAG over those members.
fn validate_request(request: &LandRequest) -> Result<(), LandingError> {
    let members: std::collections::BTreeSet<_> =
        request.members.iter().map(|member| member.pr).collect();
    if members.len() != request.members.len() {
        return Err(LandingError::Failed("duplicate landing members".into()));
    }
    let mut incoming: BTreeMap<_, usize> = members.iter().map(|pr| (*pr, 0)).collect();
    let mut edges = std::collections::BTreeSet::new();
    for edge in &request.dependency_edges {
        if !members.contains(&edge.prerequisite)
            || !members.contains(&edge.dependent)
            || !edges.insert((edge.prerequisite, edge.dependent))
        {
            return Err(LandingError::Failed(
                "dependency edges must uniquely reference landing members".into(),
            ));
        }
        if let Some(count) = incoming.get_mut(&edge.dependent) {
            *count += 1;
        }
    }
    let mut ready: Vec<_> = incoming
        .iter()
        .filter_map(|(pr, count)| (*count == 0).then_some(*pr))
        .collect();
    let mut visited = 0;
    while let Some(pr) = ready.pop() {
        visited += 1;
        for edge in request
            .dependency_edges
            .iter()
            .filter(|edge| edge.prerequisite == pr)
        {
            if let Some(count) = incoming.get_mut(&edge.dependent) {
                *count -= 1;
                if *count == 0 {
                    ready.push(edge.dependent);
                }
            }
        }
    }
    if visited != members.len() {
        return Err(LandingError::Failed(
            "landing dependencies contain a cycle".into(),
        ));
    }
    Ok(())
}

impl LandingOwner for Owner<'_> {
    fn submit(&mut self, mut request: LandRequest) -> Result<LandingId, LandingError> {
        if request.members.is_empty() {
            return Err(LandingError::Failed("no pull requests selected".into()));
        }
        let repository = self.forge.repository_identity()?;
        let target = match request.target.clone() {
            Some(target) => target,
            None => self.forge.target_branch()?,
        };
        self.pin(&mut request.members)?;
        validate_request(&request)?;
        request.target = Some(target.clone());
        let key = LandingId::request_key(&repository, &target, &request)
            .map_err(|error| LandingError::Failed(format!("key pinned request: {error}")))?;
        let previous = if self.shadow {
            None
        } else {
            latest_record(self.state, key, request.parent.as_ref())?
        };
        let changed_selection = previous
            .as_ref()
            .is_some_and(|record| selection_changed(&request, record));
        let selected_parent = previous.as_ref().map(|record| record.id.clone());
        let (mut record, capability_warning, fresh) = if let Some(record) =
            previous.filter(|_| !changed_selection)
        {
            request.generation = record.generation;
            request.parent = record.request.parent.clone();
            (record, None, false)
        } else {
            if changed_selection {
                request.parent = selected_parent;
            }
            if let Some(parent) = &request.parent {
                let parent = read_record(self.state, parent)?
                    .ok_or_else(|| LandingError::Failed("landing parent is missing".into()))?;
                if parent.repository != repository || parent.target != target {
                    return Err(LandingError::Failed(
                        "landing parent names a different repository or root target".into(),
                    ));
                }
                request.generation = parent
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| LandingError::Failed("landing generation overflow".into()))?;
            } else if request.generation != 0 {
                return Err(LandingError::Failed(
                    "a landing generation requires a parent".into(),
                ));
            }
            let effective_policy = self.policy.for_request(&request).map_err(|error| {
                LandingError::Failed(format!("resolve request policy: {error}"))
            })?;
            let (mut capabilities, mut capability_warning) = match self.forge.capabilities(&target)
            {
                Ok(capabilities) => (capabilities, None),
                Err(error) => (
                    Default::default(),
                    Some(format!("optional forge capabilities unknown: {error}")),
                ),
            };
            if capabilities
                .repository
                .as_deref()
                .is_some_and(|observed| observed != repository.as_str())
            {
                capabilities = Default::default();
                capability_warning = Some("optional forge capabilities were bound to another repository; recorded as unknown".into());
            }
            capabilities.repository = Some(repository.clone());
            let resolved_plan = effective_policy
                .plan(capabilities, &self.review_scope)
                .map_err(|error| LandingError::Failed(format!("resolve landing plan: {error}")))?;
            let id = LandingId::of(&repository, &target, &request, &resolved_plan.digest)
                .map_err(|error| LandingError::Failed(format!("key landing request: {error}")))?;
            let record = LandingRecord {
                schema_version: 2,
                id: id.clone(),
                repository,
                target: target.clone(),
                request: request.clone(),
                outcomes: request
                    .members
                    .iter()
                    .map(|member| {
                        (
                            member.pr,
                            MemberOutcome::Pending {
                                reason: "not submitted".into(),
                            },
                        )
                    })
                    .collect(),
                warnings: Vec::new(),
                generation: request.generation,
                revision: 0,
                resolved_plan,
                gate_definition: self.gate.clone(),
                progress: request
                    .members
                    .iter()
                    .map(|member| (member.pr, MemberProgress::Pending))
                    .collect(),
                operation: None,
                submission: None,
                unavailable: None,
                evidence: None,
                publication_guarantee: PublicationGuarantee::None,
                cleanup_debt: CleanupDebt::default(),
                restack_mappings: Vec::new(),
                subscribers: request.subscribers.clone(),
                outbox: Vec::new(),
                consumer_cursors: Vec::new(),
                applied_observations: Default::default(),
                passes: 0,
            };
            (record, capability_warning, true)
        };
        let selected_id = record.id.clone();
        let expected_revision = record.revision;
        if !self.shadow {
            record = allocate_generation(self.state, record, fresh, &request)?;
            if record.revision != expected_revision || (!fresh && record.id != selected_id) {
                return Err(LandingError::Pending("landing snapshot advanced while planning; resubmit to use its current generation".into()));
            }
        }
        request.generation = record.generation;
        request.parent = record.request.parent.clone();
        let effective_policy = LandPolicy::from_plan(&record.resolved_plan)
            .map_err(|error| LandingError::Failed(format!("restore landing policy: {error}")))?;
        let id = record.id.clone();
        let unavailable = record
            .resolved_plan
            .strategy
            .ensure_executable(&record.resolved_plan.plan)
            .err();
        if let Some(unavailable) = unavailable {
            let (_mutation, current) = settlement_record(self.state, record, self.shadow)?;
            record = current;
            record_interaction(&mut record, &request, expected_revision);
            for member in &request.members {
                if record
                    .outcomes
                    .get(&member.pr)
                    .is_some_and(MemberOutcome::landed)
                {
                    continue;
                }
                record.progress.insert(
                    member.pr,
                    MemberProgress::Deferred {
                        reason: unavailable.note.clone(),
                    },
                );
            }
            for outcome in record.outcomes.values_mut() {
                if outcome.landed() {
                    continue;
                }
                *outcome = MemberOutcome::Pending {
                    reason: unavailable.note.clone(),
                };
            }
            record.unavailable = Some(unavailable.clone());
            record.warnings.extend(capability_warning);
            record.revision += 1;
            if !self.shadow {
                write_record(self.state, &record)?;
            }
            return Err(LandingError::Unavailable {
                id,
                reason: Box::new(unavailable),
            });
        }
        if !request.dependency_edges.is_empty()
            && record.resolved_plan.plan.dependencies == Dependencies::Independent
        {
            return Err(LandingError::Failed(
                "dependency edges require stack dependency planning".into(),
            ));
        }
        // Landed members replay from the record; the rest advance again.
        let mut todo: Vec<LandMember> = record
            .request
            .members
            .iter()
            .filter(|member| {
                !record
                    .outcomes
                    .get(&member.pr)
                    .is_some_and(MemberOutcome::landed)
            })
            .cloned()
            .collect();
        if todo.is_empty() {
            self.last = None;
            return Ok(id);
        }
        // A request names one root target; unrelated targets use separate requests.
        todo.retain(|member| {
            let other = member.base.as_deref().filter(|base| *base != target);
            if let Some(base) = other {
                record.outcomes.insert(
                    member.pr,
                    MemberOutcome::Refused {
                        reason: format!(
                            "pull request #{} targets {base}, not the landing target {target}",
                            member.pr
                        ),
                    },
                );
            }
            other.is_none()
        });
        // The latest submission's authority and owner acceptances decide this pass.
        for member in &mut todo {
            if let Some(fresh) = request.members.iter().find(|fresh| fresh.pr == member.pr) {
                member.review_proof = fresh.review_proof.clone();
            }
        }
        self.last = None;
        let outcomes = match request.authority {
            Authority::Observe => self.observe(&todo)?,
            Authority::Policy | Authority::Chair => {
                let context = LandingContext {
                    id: id.clone(),
                    generation: record.generation,
                    request: request.clone(),
                    plan: record.resolved_plan.clone(),
                    operation: record.operation.clone(),
                    expected_revision,
                };
                // Intent precedes all network publication; the driver copies the same
                // frozen context into candidate pending markers and settled receipts.
                self.drive(
                    &todo,
                    &target,
                    context,
                    effective_policy,
                    &record.gate_definition,
                )?
            }
        };
        let (_settlement, current) = settlement_record(self.state, record, self.shadow)?;
        record = current;
        record.unavailable = None;
        let newly_landed: std::collections::BTreeSet<_> = outcomes
            .iter()
            .filter_map(|(pr, outcome)| {
                (outcome.landed() && !record.outcomes.get(pr).is_some_and(MemberOutcome::landed))
                    .then_some(*pr)
            })
            .collect();
        let stale = apply_pass_outcomes(&mut record, expected_revision, outcomes);
        record_interaction(&mut record, &request, expected_revision);
        for (pr, outcome) in &record.outcomes {
            match outcome {
                MemberOutcome::Landed { .. } => {
                    record.progress.remove(pr);
                }
                MemberOutcome::Pending { .. } => {
                    record.progress.insert(*pr, MemberProgress::Pending);
                }
                MemberOutcome::Unknown { reason } => {
                    record.progress.insert(
                        *pr,
                        MemberProgress::Unknown {
                            reason: reason.clone(),
                        },
                    );
                }
                MemberOutcome::Refused { reason } => {
                    record.progress.insert(
                        *pr,
                        MemberProgress::Failed {
                            reason: reason.clone(),
                        },
                    );
                }
            }
        }
        if !stale && let Some((report, _)) = &self.last {
            record.warnings = report.warnings.clone();
        }
        record.warnings.extend(capability_warning);
        record.passes += 1;
        record.revision += 1;
        if (!stale || !newly_landed.is_empty())
            && let Some((report, failure)) = &self.last
        {
            record.cleanup_debt.items.clear();
            if let Some(LandingError::LocalFollowUp(reason)) = failure {
                record.cleanup_debt.items.push(CleanupDebtItem {
                    step: "local_follow_up".into(),
                    reason: reason.clone(),
                    attempts: u32::try_from(record.passes).unwrap_or(u32::MAX),
                });
            }
            let mut evidence = record.evidence.clone().unwrap_or_default();
            for receipt in &report.receipts {
                evidence.candidate_commit = Some(receipt.candidate.clone());
                evidence.candidate_tree = Some(receipt.tree.clone());
                evidence.target_base = Some(receipt.base.clone());
                for step in &receipt.candidate_steps {
                    if stale && !newly_landed.contains(&step.pr.number) {
                        continue;
                    }
                    evidence
                        .members
                        .retain(|member| member.pr != step.pr.number);
                    evidence
                        .members
                        .push(provenant_records::landing::MemberEvidence {
                            pr: step.pr.number,
                            head: step.pr.head.clone(),
                            base: step.pr.base_branch.clone(),
                            base_commit: receipt.base.clone(),
                            gate_definition_digest: receipt.gate_definition.digest.clone(),
                            candidate_commit: receipt.candidate.clone(),
                            candidate_tree: receipt.tree.clone(),
                            checks: receipt.gate_evidence.checks.clone(),
                        });
                }
                for pr in &receipt.pull_requests {
                    if stale && !newly_landed.contains(&pr.number) {
                        continue;
                    }
                    if let Some(commit) = receipt.approved_to_landed.get(&pr.head) {
                        evidence.member_commits.insert(pr.number, commit.clone());
                    }
                }
                let guarantee = match receipt.publish.mode {
                    super::model::PublishMode::FastForward | super::model::PublishMode::Command => {
                        PublicationGuarantee::ExactCandidate
                    }
                    super::model::PublishMode::RestOrdered => PublicationGuarantee::OrderedMembers,
                };
                if record.publication_guarantee != PublicationGuarantee::OrderedMembers {
                    record.publication_guarantee = guarantee;
                }
            }
            if !evidence.member_commits.is_empty() {
                record.submission = Some(Submission::Merged {
                    evidence: evidence.clone(),
                });
                record.evidence = Some(evidence);
            }
        }
        // Debt is an independent inventory, including reported cleanup refusals
        // that did not turn the publication itself into a failed result.
        match receipt::pending(self.state, &record.repository, &target) {
            Ok(attempts) => {
                let mut pending = BTreeMap::new();
                for attempt in attempts.into_iter().filter(|attempt| {
                    attempt
                        .policy
                        .landing
                        .as_ref()
                        .is_some_and(|context| context.id == id)
                }) {
                    if let Some(settled) = attempt.settled {
                        for (step, needed) in [
                            ("local_follow_up", settled.local_follow_pending),
                            ("candidate_ref", settled.cleanup_pending),
                        ] {
                            if needed {
                                let item = record
                                    .cleanup_debt
                                    .items
                                    .iter()
                                    .find(|item| item.step == step)
                                    .cloned()
                                    .unwrap_or_else(|| CleanupDebtItem {
                                        step: step.into(),
                                        reason: format!(
                                            "pending cleanup for {}",
                                            attempt.candidate.ref_name
                                        ),
                                        attempts: u32::try_from(record.passes).unwrap_or(u32::MAX),
                                    });
                                pending.entry(step).or_insert(item);
                            }
                        }
                    }
                }
                record.cleanup_debt.items = pending.into_values().collect();
            }
            Err(error) => record
                .warnings
                .push(format!("cleanup debt inventory unavailable: {error}")),
        }
        let event = LandingChanged {
            id: id.clone(),
            generation: record.generation,
            revision: record.revision,
            operation: record.operation.clone(),
            outcomes: record.outcomes.clone(),
            evidence: record.evidence.clone(),
            deduplication_key: format!("landing:{id}:{}:{}", record.generation, record.revision),
        };
        record.outbox.push(OutboxEntry {
            event,
            delivered_to: Default::default(),
            attempts: 0,
        });
        if !self.shadow {
            write_record(self.state, &record)?;
        }
        if stale {
            self.last = None;
        }
        Ok(id)
    }

    fn record(&self, id: &LandingId) -> Result<Option<LandingRecord>, LandingError> {
        read_record(self.state, id)
    }
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    //! Invariants at the port: one record per landing with per-member
    //! outcomes, idempotent resubmission, and a restart that resumes.
    use super::*;
    use crate::pr::landing::fakes::*;
    use crate::pr::landing::model::GateResult;
    use crate::pr::landing::policy::LandPolicy;
    use provenant_cli::domains::{ApprovalSource, ApprovalTier, LandOverride, LandSettings};
    use provenant_records::landing::PolicyChoice;

    #[derive(Default)]
    struct ScopedFakeReviews {
        records: FakeReviews,
        seen: std::cell::RefCell<Vec<provenant_records::landing::ReviewScope>>,
    }
    impl ReviewRecordSource for ScopedFakeReviews {
        fn reviews(
            &self,
            scope: &provenant_records::landing::ReviewScope,
            number: u64,
            head: &str,
        ) -> Result<super::super::ports::Reviews, LandingError> {
            self.seen.borrow_mut().push(scope.clone());
            super::super::ports::ReviewRecords::reviews(&self.records, number, head)
        }
    }

    struct CapturingGate {
        inner: FakeGate,
        polls: Vec<u64>,
    }
    impl GateRunner for CapturingGate {
        fn gate(
            &mut self,
            candidate: &super::super::model::Candidate,
            definition: &GateDefinition,
            poll_seconds: u64,
        ) -> Result<super::super::model::GateEvidence, LandingError> {
            self.polls.push(poll_seconds);
            self.inner.gate(candidate, definition, poll_seconds)
        }
    }

    struct World {
        forge: FakeForge,
        repo: FakeRepo,
        gate: CapturingGate,
        alarms: FakeAlarms,
        settings: LandSettings,
        policy: LandPolicy,
        definition: GateDefinition,
        state: tempfile::TempDir,
        reviews: ScopedFakeReviews,
        review_scope: provenant_records::landing::ReviewScope,
    }

    fn world(count: u64) -> Result<World, Box<dyn std::error::Error>> {
        let (forge, repo, gate, mut settings, _definition) =
            crate::pr::landing::driver::tests::fixture(count);
        settings.approval_sources = vec![ApprovalSource::Forge];
        settings.approval_tier = ApprovalTier::Carry;
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        let definition = super::super::gate_definition(&settings.gate)?;
        Ok(World {
            forge,
            repo,
            gate: CapturingGate {
                inner: gate,
                polls: Vec::new(),
            },
            alarms: FakeAlarms::default(),
            settings,
            policy,
            definition,
            state: tempfile::tempdir()?,
            reviews: ScopedFakeReviews::default(),
            review_scope: Default::default(),
        })
    }

    fn submit(
        world: &mut World,
        request: LandRequest,
    ) -> Result<(LandingId, LandingRecord), Box<dyn std::error::Error>> {
        let mut owner = Owner {
            forge: &mut world.forge,
            repo: &mut world.repo,
            runner: &mut world.gate,
            alarms: &mut world.alarms,
            state: world.state.path(),
            policy: &world.policy,
            gate: &world.definition,
            reviews: &world.reviews,
            review_scope: world.review_scope.clone(),
            shadow: false,
            last: None,
        };
        let id = owner.submit(request)?;
        let record = owner.record(&id)?.ok_or("no landing record")?;
        Ok((id, record))
    }

    fn members(numbers: &[u64]) -> Vec<LandMember> {
        numbers
            .iter()
            .map(|pr| LandMember {
                pr: *pr,
                head: None,
                base: None,
                review_proof: ReviewProof::Policy,
            })
            .collect()
    }

    #[test]
    fn superseded_generation_or_revision_never_reaches_publication()
    -> Result<(), Box<dyn std::error::Error>> {
        for new_generation in [false, true] {
            let mut world = world(1)?;
            let (_, snapshot) = submit(
                &mut world,
                LandRequest {
                    members: members(&[1]),
                    authority: Authority::Observe,
                    ..Default::default()
                },
            )?;
            let context = LandingContext {
                id: snapshot.id.clone(),
                generation: snapshot.generation,
                request: snapshot.request.clone(),
                plan: snapshot.resolved_plan.clone(),
                operation: snapshot.operation.clone(),
                expected_revision: snapshot.revision,
            };
            {
                let _lock = receipt::lock(world.state.path())?;
                fence(world.state.path(), &context)?;
                let mut newer = snapshot.clone();
                if new_generation {
                    newer.generation += 1;
                    newer.request.generation = newer.generation;
                    newer.request.parent = Some(snapshot.id.clone());
                    newer.id = LandingId::of(
                        &newer.repository,
                        &newer.target,
                        &newer.request,
                        &newer.resolved_plan.digest,
                    )?;
                } else {
                    newer.revision += 1;
                }
                write_record(world.state.path(), &newer)?;
            }
            let mut policy = world.policy.clone();
            policy.landing = Some(context);
            let result = driver::run(
                &mut world.forge,
                &mut world.repo,
                &mut world.gate,
                &mut world.alarms,
                driver::Request {
                    numbers: &[1],
                    target: &snapshot.target,
                    settings: &world.settings,
                    policy: &policy,
                    gate: &world.definition,
                    shadow: false,
                    state: world.state.path(),
                    approvals: Approvals::forge(),
                    pins: &BTreeMap::new(),
                },
            );
            assert!(
                matches!(result, Err(LandingError::RunFailed { cause, .. }) if matches!(*cause, LandingError::Pending(_)))
            );
            assert!(world.forge.merges.is_empty() && world.repo.push_attempts.is_empty());
            assert!(world.gate.polls.is_empty());
        }
        Ok(())
    }

    #[test]
    fn stored_record_roundtrips_pinned_plan_and_event_delivery()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_records::landing::{ConsumerCursor, Subscriber};
        let mut world = world(1)?;
        let subscriber = Subscriber {
            run_id: Some("run".into()),
            node_id: Some("land".into()),
            session_id: None,
        };
        let (_, mut record) = submit(
            &mut world,
            LandRequest {
                members: members(&[1]),
                subscribers: vec![subscriber.clone()],
                ..Default::default()
            },
        )?;
        record.consumer_cursors.push(ConsumerCursor {
            subscriber: subscriber.clone(),
            revision: record.revision,
        });
        record.outbox[0].delivered_to.insert(subscriber);
        let decoded: LandingRecord = serde_json::from_slice(&serde_json::to_vec(&record)?)?;
        assert_eq!(decoded, record);
        assert!(
            record
                .request
                .members
                .iter()
                .all(|member| member.head.is_some() && member.base.is_some())
        );
        assert!(!record.resolved_plan.digest.is_empty());
        Ok(())
    }

    #[test]
    fn saved_policy_and_gate_cannot_change_under_the_same_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(1)?;
        let (id, record) = submit(
            &mut world,
            LandRequest {
                members: members(&[1]),
                ..Default::default()
            },
        )?;
        let mut scope_changed = record.clone();
        scope_changed.resolved_plan.provenance.insert(
            "review_scope".into(),
            serde_json::json!({"project":null,"exempt_patterns":["src/"]}),
        );
        let mut gate_changed = record;
        gate_changed
            .gate_definition
            .checks
            .push("different-check".into());
        for changed in [scope_changed, gate_changed] {
            write_record(world.state.path(), &changed)?;
            assert!(read_record(world.state.path(), &id).is_err());
        }
        Ok(())
    }

    #[test]
    fn stale_passes_preserve_newer_decisions_and_confirmed_publication()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(1)?;
        world.gate.inner.check_result = Some(GateResult::Pending);
        let (_, snapshot) = submit(
            &mut world,
            LandRequest {
                members: members(&[1]),
                policy: PolicyChoice {
                    patience_seconds: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            },
        )?;
        let mut older = snapshot.request.clone();
        older.authority = Authority::Chair;
        older.members[0].review_proof = ReviewProof::Accepted {
            reason: "older acceptance".into(),
        };
        let mut current = snapshot.clone();
        current.revision += 1;
        current.request.authority = Authority::Observe;
        let saved = current.request.clone();
        record_interaction(&mut current, &older, snapshot.revision);
        assert_eq!(current.request, saved);
        for current in [
            MemberOutcome::Refused {
                reason: "closed".into(),
            },
            MemberOutcome::Unknown {
                reason: "dispatch unknown".into(),
            },
        ] {
            let mut record = snapshot.clone();
            record.revision += 1;
            record.outcomes.insert(1, current.clone());
            assert!(apply_pass_outcomes(
                &mut record,
                snapshot.revision,
                BTreeMap::from([(
                    1,
                    MemberOutcome::Pending {
                        reason: "older read".into()
                    }
                )])
            ));
            assert_eq!(record.outcomes[&1], current);
            assert!(apply_pass_outcomes(
                &mut record,
                snapshot.revision,
                BTreeMap::from([(
                    1,
                    MemberOutcome::Landed {
                        commit: Some("confirmed".into())
                    }
                )])
            ));
            assert!(record.outcomes[&1].landed());
        }
        Ok(())
    }

    #[test]
    fn a_single_pull_request_lands_into_one_record() -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(1)?;
        let (id, record) = submit(
            &mut world,
            LandRequest {
                members: members(&[1]),
                ..Default::default()
            },
        )?;
        assert_eq!(record.id, id);
        assert_eq!(record.outcomes.len(), 1);
        assert!(record.outcomes[&1].landed(), "{record:?}");
        // The pinned head is part of the record.
        assert!(record.request.members[0].head.is_some());
        Ok(())
    }

    #[test]
    fn explicit_owner_acceptance_lands_members_into_one_record()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(2)?;
        world.forge.approvals_at_head.clear();
        let mut members = members(&[1, 2]);
        for member in &mut members {
            member.review_proof = ReviewProof::Accepted {
                reason: "owner accepts uncovered delta".into(),
            };
        }
        let (_, record) = submit(
            &mut world,
            LandRequest {
                members,
                authority: Authority::Chair,
                policy: PolicyChoice::default(),
                ..Default::default()
            },
        )?;
        assert_eq!(record.outcomes.len(), 2);
        assert!(
            record.outcomes.values().all(MemberOutcome::landed),
            "{record:?}"
        );
        Ok(())
    }

    #[test]
    fn resubmission_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(1)?;
        let request = LandRequest {
            members: members(&[1]),
            ..Default::default()
        };
        let (first, _) = submit(&mut world, request.clone())?;
        let merges = world.forge.merges.len();
        let pushes = world.repo.push_attempts.len();
        let (again, record) = submit(&mut world, request)?;
        assert_eq!(first, again);
        assert_eq!(world.forge.merges.len(), merges);
        assert_eq!(world.repo.push_attempts.len(), pushes);
        assert_eq!(record.passes, 1);
        let (from_stored_request, replayed) = submit(&mut world, record.request.clone())?;
        assert_eq!(from_stored_request, first);
        assert_eq!(replayed.passes, record.passes);
        assert_eq!(world.forge.merges.len(), merges);
        assert_eq!(world.repo.push_attempts.len(), pushes);
        Ok(())
    }

    #[test]
    fn a_restart_resumes_a_pending_member() -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(1)?;
        world
            .settings
            .approval_sources
            .insert(0, ApprovalSource::ProvenantReview);
        world.policy = LandPolicy::resolve(
            &world.settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        world.definition.timeout_seconds = 0;
        world.gate.inner.check_result = Some(GateResult::Pending);
        let request = LandRequest {
            members: members(&[1]),
            policy: PolicyChoice {
                patience_seconds: Some(0),
                ..Default::default()
            },
            ..Default::default()
        };
        let (id, record) = submit(&mut world, request.clone())?;
        assert!(
            matches!(record.outcomes[&1], MemberOutcome::Pending { .. }),
            "{record:?}"
        );
        // A new process must use the saved plan and gate despite config changes.
        let saved_gate = record.gate_definition.clone();
        let saved_scope = LandPolicy::review_scope(&record.resolved_plan)?;
        let prior_reviews = world.reviews.seen.borrow().len();
        world.review_scope.exempt_patterns = vec!["src/".into()];
        world.review_scope.project = Some(provenant_core::ProjectId::from_bytes([9; 16]));
        world.settings.strategy = provenant_records::landing::Strategy::Async;
        world.settings.approval_tier = ApprovalTier::None;
        world.settings.approval_sources = vec![ApprovalSource::Forge];
        world.policy = LandPolicy::resolve(
            &world.settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        world.definition.checks = vec!["different-check".into()];
        world.definition.digest = "different-definition".into();
        world.gate.inner.check_result = None;
        let mut request = request;
        request.policy.patience_seconds = Some(30);
        request.policy.poll_seconds = Some(1);
        let choice = request.policy.clone();
        let (resumed, record) = submit(&mut world, request)?;
        assert_eq!(record.request.policy, choice);
        assert_eq!(record.gate_definition, saved_gate);
        assert_eq!(
            LandPolicy::review_scope(&record.resolved_plan)?,
            saved_scope
        );
        assert_eq!(world.gate.polls.last(), Some(&1));
        let seen = world.reviews.seen.borrow();
        assert!(seen.len() > prior_reviews);
        assert!(seen.iter().all(|scope| scope == &saved_scope));
        assert_eq!(
            record.resolved_plan.strategy,
            provenant_records::landing::Strategy::Batch
        );
        assert_eq!(
            LandPolicy::from_plan(&record.resolved_plan)?
                .settings()?
                .approval_tier,
            ApprovalTier::Carry
        );
        assert_eq!(resumed, id);
        assert!(record.outcomes[&1].landed(), "{record:?}");
        assert_eq!(record.passes, 2);
        Ok(())
    }

    #[test]
    fn explicit_replanning_links_a_generation_and_replays_it()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_records::landing::Strategy;
        let mut world = world(1)?;
        world.definition.timeout_seconds = 0;
        world.gate.inner.check_result = Some(GateResult::Pending);
        let request = LandRequest {
            members: members(&[1]),
            policy: PolicyChoice {
                patience_seconds: Some(0),
                ..Default::default()
            },
            ..Default::default()
        };
        let (first, first_record) = submit(&mut world, request.clone())?;
        let replan = LandRequest {
            strategy: Some(Strategy::Async),
            parent: Some(first.clone()),
            ..request
        };
        assert!(submit(&mut world, replan.clone()).is_err());
        let child = latest_record(world.state.path(), first.request, Some(&first))?
            .ok_or("missing child")?;
        assert_eq!(child.generation, first_record.generation + 1);
        assert_eq!(child.request.parent.as_ref(), Some(&first));
        assert_eq!(child.resolved_plan.strategy, Strategy::Async);
        assert!(child.unavailable.is_some());
        assert!(submit(&mut world, replan).is_err());
        let replayed = latest_record(world.state.path(), first.request, Some(&first))?
            .ok_or("missing child")?;
        assert_eq!(replayed.id, child.id);
        assert_eq!(replayed.generation, child.generation);
        assert!(world.forge.merges.is_empty());
        assert_eq!(
            read_record(world.state.path(), &first)?.ok_or("missing parent")?,
            first_record
        );
        Ok(())
    }

    #[test]
    fn independently_planned_replans_allocate_one_generation_chain()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_records::landing::Strategy;
        let mut world = world(1)?;
        world.definition.timeout_seconds = 0;
        world.gate.inner.check_result = Some(GateResult::Pending);
        let (parent_id, parent) = submit(
            &mut world,
            LandRequest {
                members: members(&[1]),
                policy: PolicyChoice {
                    patience_seconds: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            },
        )?;
        let proposal = |strategy| -> Result<LandingRecord, Box<dyn std::error::Error>> {
            let mut record = parent.clone();
            record.request.strategy = Some(strategy);
            record.request.parent = Some(parent_id.clone());
            record.generation += 1;
            record.request.generation = record.generation;
            record.resolved_plan = world.policy.for_request(&record.request)?.plan(
                parent.resolved_plan.capabilities.clone(),
                &world.review_scope,
            )?;
            record.id = LandingId::of(
                &record.repository,
                &record.target,
                &record.request,
                &record.resolved_plan.digest,
            )?;
            record.revision = 0;
            record.passes = 0;
            record.outbox.clear();
            Ok(record)
        };
        // Both proposals observe the same parent before either mutation commits.
        let first = proposal(Strategy::Async)?;
        let second = proposal(Strategy::Queue)?;
        let first_request = first.request.clone();
        let first = allocate_generation(world.state.path(), first, true, &first_request)?;
        let second_request = second.request.clone();
        let second = allocate_generation(world.state.path(), second, true, &second_request)?;
        assert_eq!(second.generation, first.generation + 1);
        assert_eq!(second.request.parent.as_ref(), Some(&first.id));
        assert_eq!(
            latest_record(world.state.path(), parent_id.request, None)?
                .ok_or("missing current generation")?
                .id,
            second.id
        );
        assert!(read_record(world.state.path(), &first.id)?.is_some());
        Ok(())
    }

    #[test]
    fn observation_cannot_land_a_member_retargeted_to_another_root()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(1)?;
        let mut pinned = members(&[1]);
        pinned[0].head = Some(format!("{:040x}", 1));
        pinned[0].base = Some("main".into());
        let info = world
            .forge
            .pull_requests
            .get_mut(&1)
            .ok_or("missing pull request")?;
        info.base_branch = "release".into();
        info.state = PullRequestState::Merged;
        let (_, record) = submit(
            &mut world,
            LandRequest {
                members: pinned,
                authority: Authority::Observe,
                ..Default::default()
            },
        )?;
        assert!(matches!(record.outcomes[&1], MemberOutcome::Refused { .. }));
        assert!(world.forge.merges.is_empty());
        Ok(())
    }

    #[test]
    fn a_moved_head_is_refused_not_landed() -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(1)?;
        let mut members = members(&[1]);
        members[0].head = Some(format!("{:040x}", 99));
        let (_, record) = submit(
            &mut world,
            LandRequest {
                members,
                ..Default::default()
            },
        )?;
        assert!(
            matches!(&record.outcomes[&1], MemberOutcome::Refused { reason } if reason.contains("HEAD_MOVED")),
            "{record:?}"
        );
        assert!(world.forge.merges.is_empty());
        Ok(())
    }
}
