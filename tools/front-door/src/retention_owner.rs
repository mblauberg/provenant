//! Application feeds and the retirement seam consumed by the worktree owner.
//! S2 replaces this persistence seam, not the collector's evidence policy.
use provenant_confinement::worktrees::{
    self, ForgeObservation, RetirementAction, RetirementOwner, WorktreeContext, WorktreeError,
    WorktreeInfo,
};
use provenant_core::{CancelToken, GitOid, Roots};
use provenant_fabric_store::{HostStore, Request};
use provenant_os::{clock::SystemClock, exec::Termination, fs::PublishMode};
use provenant_records::{custody::*, landing::*};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    ffi::OsStr,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Deserialize)]
struct Repo {
    full_name: String,
}
#[derive(Clone, Deserialize)]
struct Branch {
    #[serde(rename = "ref")]
    name: String,
    sha: GitOid,
    repo: Option<Repo>,
}
#[derive(Clone, Deserialize)]
struct Pull {
    number: u64,
    state: String,
    merged_at: Option<String>,
    merge_commit_sha: Option<GitOid>,
    head: Branch,
    base: Branch,
}

pub(crate) struct Owner<'a> {
    roots: &'a Roots,
    home: &'a Path,
    project: &'a Path,
    pulls: RefCell<Option<Result<Vec<Pull>, String>>>,
    observed_merge: Option<LandedEvidence>,
}
impl<'a> Owner<'a> {
    pub(crate) fn new(roots: &'a Roots, home: &'a Path, project: &'a Path) -> Self {
        Self {
            roots,
            home,
            project,
            pulls: RefCell::new(None),
            observed_merge: None,
        }
    }

    pub(crate) fn with_merge_observation(mut self, evidence: &LandedEvidence) -> Self {
        self.observed_merge = Some(evidence.clone());
        self
    }

    /// Recover the one lifecycle by custody pointer, even when its worktree no
    /// longer appears in inventory. No directory-name discovery is authority.
    pub(crate) fn recover(
        &self,
        context: &WorktreeContext<'_>,
    ) -> Result<Vec<RemovalRecord>, WorktreeError> {
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let Some(host) =
            HostStore::open_read_only(&self.roots.state, Instant::now() + Duration::from_secs(2))
                .map_err(|_| WorktreeError::UnknownWriter)?
        else {
            return Ok(Vec::new());
        };
        let mut query = CustodyQuery {
            project: Some(context.binding.project_id()),
            state: Some(StateKind::Retired),
            ..Default::default()
        };
        let mut ids = std::collections::BTreeSet::new();
        loop {
            let page = host
                .custody(&query, &call)
                .map_err(|_| WorktreeError::UnknownWriter)?;
            for item in page.items {
                if let CustodyState::Retired { retirement_id } = item.state {
                    ids.insert(retirement_id);
                }
            }
            match page.next {
                Some(next) if next > query.after => query.after = next,
                Some(_) => return Err(WorktreeError::InvalidOutput),
                None => break,
            }
        }
        let mut removals = Vec::new();
        if ids.is_empty() {
            return Ok(removals);
        }
        let directory = context
            .project_state
            .open_verified_directory(OsStr::new("retirements"))?;
        for id in ids {
            let raw = directory.read_bounded(OsStr::new(&format!("{id}.json")), 64 * 1024)?;
            let record: RetirementRecord =
                serde_json::from_slice(&raw).map_err(|_| WorktreeError::InvalidOutput)?;
            if record.id != id
                || record.reference.repository != context.binding.project_id().to_string()
            {
                return Err(WorktreeError::IdentityChanged);
            }
            if !record.outcomes.iter().any(|step| {
                matches!(
                    step.state,
                    RetirementStepState::Pending | RetirementStepState::Unknown { .. }
                )
            }) {
                continue;
            }
            let tip: GitOid = record
                .expected_tip
                .parse()
                .map_err(|_| WorktreeError::InvalidOutput)?;
            let worktree = record
                .outcomes
                .iter()
                .find_map(|s| s.step.strip_prefix("worktree:"))
                .map(std::path::PathBuf::from);
            if let Some(path) = &worktree {
                if !path.is_absolute() {
                    return Err(WorktreeError::InvalidOutput);
                }
                match std::fs::symlink_metadata(path) {
                    Ok(_) => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => continue,
                }
            }
            let reference = if record.reference.reference.starts_with("refs/heads/") {
                Some(
                    provenant_confinement::repo::GitRefName::new(&record.reference.reference)
                        .map_err(|_| WorktreeError::InvalidOutput)?,
                )
            } else {
                None
            };
            let lane = WorktreeInfo {
                warnings: Vec::new(),
                path: worktree
                    .clone()
                    .unwrap_or_else(|| context.repo.root_path().into()),
                branch: reference.as_ref().map(|r| r.as_str().to_owned()),
                head: tip,
                dirty: worktrees::WorktreeCondition::Clean,
                owner: Ok(None),
                primary: worktree.is_none(),
                occupancy: worktrees::Occupancy::Available,
            };
            self.custody(&lane)
                .map_err(|detail| WorktreeError::UnknownOccupancy {
                    path: lane.path.clone(),
                    detail,
                })?;
            let action = if record.decision.action
                == provenant_records::landing::RetirementAction::Preserve
            {
                RetirementAction::Idle
            } else {
                RetirementAction::Subsumed(
                    serde_json::from_str(&record.decision.reason)
                        .map_err(|_| WorktreeError::InvalidOutput)?,
                )
            };
            if let (Some(reference), RetirementAction::Subsumed(evidence)) = (&reference, &action) {
                if let Some(actual) = worktrees::ref_tip(context, reference, &cancel)? {
                    let refs =
                        worktrees::retained_refs(context, Some(reference), None, &[], &cancel)?;
                    let ledger = worktrees::RefLedger::observe_context(context)?
                        .ok_or(WorktreeError::UnknownWriter)?;
                    let owned = ledger.load(&worktrees::RefEndpoint::Local, reference)?;
                    if actual == tip
                        && owned
                            .as_ref()
                            .is_some_and(|r| r.generation == record.reference.generation)
                    {
                        let eligible = || {
                            worktrees::validate_removal_evidence(
                                context, tip, evidence, &refs, &cancel,
                            )
                        };
                        let _ = worktrees::delete_ref_leased_when(
                            context, reference, tip, &cancel, &eligible,
                        )?;
                    }
                }
            }
            // Complete only factual steps; record() independently leaves a
            // moved/checked-out branch pending or refused, never landed.
            let completed = self.record(context, &lane, &action, true, Some(&id))?;
            if let Some(path) = worktree {
                if let Some(removal) = self.projection(
                    context,
                    &completed,
                    Subject::new(SubjectKind::Worktree, path.to_string_lossy()),
                )? {
                    removals.push(removal);
                }
            }
            if let Some(reference) = reference {
                if let Some(removal) = self.projection(
                    context,
                    &completed,
                    Subject::new(SubjectKind::Branch, reference.as_str()),
                )? {
                    removals.push(removal);
                }
            }
        }
        Ok(removals)
    }
    fn pulls(&self, fresh: bool) -> Result<Vec<Pull>, String> {
        if fresh {
            self.pulls.borrow_mut().take();
        }
        if self.pulls.borrow().is_none() {
            let result = (|| {
                let result = crate::pr::execute_for(
                    self.project,
                    &[
                        "gh",
                        "api",
                        "--method",
                        "GET",
                        "--paginate",
                        "--slurp",
                        "repos/{owner}/{repo}/pulls",
                        "-f",
                        "state=all",
                        "-f",
                        "per_page=100",
                    ],
                    Duration::from_secs(15),
                    false,
                )
                .map_err(|e| e.to_string())?;
                if result.termination != Termination::Exit(0)
                    || result.timed_out
                    || result.stdout.truncated
                    || result.drain_truncated
                    || result.stdout.read_failed
                    || result.stdout.sink_failed
                {
                    return Err(
                        "forge snapshot unavailable or incomplete; last fetched refs only".into(),
                    );
                }
                let pages: Vec<Vec<Pull>> = serde_json::from_slice(&result.stdout.bytes)
                    .map_err(|e| format!("unreadable forge snapshot: {e}"))?;
                let pulls: Vec<_> = pages.into_iter().flatten().collect();
                if pulls.iter().any(|p| {
                    !matches!(p.state.as_str(), "open" | "closed")
                        || p.head.repo.is_none()
                        || p.base.repo.is_none()
                }) {
                    return Err("forge branch identity or state is unknown".into());
                }
                Ok(pulls)
            })();
            *self.pulls.borrow_mut() = Some(result);
        }
        self.pulls
            .borrow()
            .as_ref()
            .ok_or_else(|| "forge not observed".to_string())?
            .clone()
    }
    pub(crate) fn remote_safe(
        &self,
        branch: &str,
        tip: GitOid,
        min_days: u64,
        context: &WorktreeContext<'_>,
    ) -> Result<(), String> {
        // Re-read immediately before the leased push, including PR base users.
        if self
            .pulls(true)?
            .iter()
            .any(|p| p.state == "open" && (p.head.name == branch || p.base.name == branch))
        {
            return Err("open PR uses the remote branch as head or base".into());
        }
        let result = crate::gc_support::host_git_with_stderr(
            self.project,
            context.executor,
            &["show", "-s", "--format=%ct", &tip.to_string(), "--"],
            Duration::from_secs(5),
            false,
        )
        .map_err(|e| e.to_string())?;
        if result.termination != Termination::Exit(0) {
            return Err("remote tip age unavailable".into());
        }
        let at: i64 = std::str::from_utf8(&result.stdout.bytes)
            .map_err(|e| e.to_string())?
            .trim()
            .parse()
            .map_err(|e| format!("tip age: {e}"))?;
        use provenant_core::Clock;
        let now = context
            .clock
            .wall_now()
            .map_err(|e| e.to_string())?
            .as_micros()
            / 1_000_000;
        if now.saturating_sub(at)
            < i64::try_from(min_days.saturating_mul(86400)).unwrap_or(i64::MAX)
        {
            return Err("remote tip is younger than gc.projects.remote_min_tip_age_days".into());
        }
        Ok(())
    }
    fn context(&self) -> Result<provenant_cli::fabric::Context, String> {
        provenant_cli::fabric::Context::open_read_only(self.roots.clone(), self.home, self.project)
            .map_err(|e| e.to_string())
    }
    fn subjects(lane: &WorktreeInfo) -> Vec<Subject> {
        let mut subjects = if lane.primary {
            Vec::new()
        } else {
            vec![Subject::new(
                SubjectKind::Worktree,
                lane.path.to_string_lossy(),
            )]
        };
        if let Some(branch) = &lane.branch {
            subjects.push(Subject::new(SubjectKind::Branch, branch));
        }
        subjects
    }
}
impl RetirementOwner for Owner<'_> {
    fn custody(&self, lane: &WorktreeInfo) -> Result<(), String> {
        let context = self.context()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let Some(ledger) =
            HostStore::open_read_only(&self.roots.state, Instant::now() + Duration::from_secs(2))
                .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let subjects = Self::subjects(lane);
        let mut query = CustodyQuery {
            project: Some(context.identity.project),
            ..Default::default()
        };
        loop {
            let page = ledger.custody(&query, &call).map_err(|e| e.to_string())?;
            for record in page
                .items
                .iter()
                .filter(|r| r.state == CustodyState::Active)
            {
                if subjects
                    .iter()
                    .any(|s| *s == record.subject || record.links.contains(s))
                {
                    match crate::custody::presence(
                        record,
                        self.roots,
                        context.identity.inbox.host,
                        context.identity.inbox.actor,
                    ) {
                        HolderPresence::Unattended => {}
                        HolderPresence::Live => {
                            return Err("live custody holder retains this item".into());
                        }
                        HolderPresence::Unknown => {
                            return Err("custody holder presence is unknown".into());
                        }
                    }
                }
            }
            match page.next {
                Some(next) if next > query.after => query.after = next,
                Some(_) => return Err("custody cursor did not advance".into()),
                None => break,
            }
        }
        Ok(())
    }
    fn forge(&self, branch: &provenant_confinement::repo::GitRefName) -> ForgeObservation {
        if let Some(LandedEvidence {
            source:
                LandingSource::ForgeMerged {
                    pr,
                    head,
                    merge_commit,
                },
            ..
        }) = &self.observed_merge
        {
            return ForgeObservation::Merged {
                pr: pr.clone(),
                head: *head,
                merge_commit: *merge_commit,
            };
        }
        let pulls = match self.pulls(false) {
            Ok(p) => p,
            Err(error) => return ForgeObservation::Unknown(error),
        };
        let mut matching: Vec<_> = pulls
            .into_iter()
            .filter(|p| {
                p.head.name == branch.short_name()
                    && p.head.repo.as_ref().map(|r| &r.full_name)
                        == p.base.repo.as_ref().map(|r| &r.full_name)
            })
            .collect();
        if let Some(open) = matching.iter().find(|p| p.state == "open") {
            return ForgeObservation::Open {
                base: format!("refs/heads/{}", open.base.name),
            };
        }
        // A reused branch's most recent PR owns its recorded head. A moved tip
        // will fail the collector's exact-head abandonment/merge comparison.
        matching.sort_by_key(|p| p.number);
        match matching.pop() {
            Some(p) if p.merged_at.is_some() => ForgeObservation::Merged {
                pr: p.number.to_string(),
                head: p.head.sha,
                merge_commit: p.merge_commit_sha,
            },
            Some(p) => ForgeObservation::Closed { head: p.head.sha },
            None => ForgeObservation::Absent,
        }
    }
    fn integration_bases(&self) -> Vec<(String, GitOid)> {
        if self.observed_merge.is_some() {
            return Vec::new();
        }
        self.pulls(false)
            .map(|pulls| {
                pulls
                    .into_iter()
                    .filter(|p| p.state == "open")
                    .map(|p| (p.base.name, p.base.sha))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn record(
        &self,
        context: &WorktreeContext<'_>,
        lane: &WorktreeInfo,
        action: &RetirementAction,
        completed: bool,
        prior_id: Option<&str>,
    ) -> Result<String, WorktreeError> {
        // This function is the sole replaceable S2 persistence seam. Reports
        // and custody point to this frozen RetirementRecord; no second record.
        let project = context.binding.project_id();
        let ledger = worktrees::RefLedger::for_context(context)?;
        let now = context
            .clock
            .wall_now()
            .map_err(|_| WorktreeError::InvalidOutput)?;
        let mut ownership = ledger.list()?.into_iter().find(|r| {
            r.endpoint == worktrees::RefEndpoint::Local
                && lane.branch.as_ref() == Some(&r.reference)
        });
        if prior_id.is_none() && ownership.is_none() {
            if let Some(reference) = &lane.branch {
                ownership = Some(
                    ledger.record(
                        worktrees::RefObservation {
                            endpoint: worktrees::RefEndpoint::Local,
                            reference: provenant_confinement::repo::GitRefName::new(reference)
                                .map_err(|_| WorktreeError::InvalidRequest)?,
                            origin: worktrees::RefOrigin::Adopted,
                            tip: lane.head,
                            recorded_at: now,
                            attempt_id: None,
                            operation_id: None,
                        },
                        context.clock,
                        &CancelToken::new(),
                    )?,
                );
            }
        }
        let generation = if let Some(owned) = &ownership {
            owned.generation
        } else if prior_id.is_none() {
            provenant_confinement::verify_worktree(context.repo, &lane.path)?
                .identity()
                .inode
        } else {
            0
        };
        let reference = RetirementRef {
            repository: project.to_string(),
            endpoint: RetirementEndpoint::Local,
            reference: lane
                .branch
                .clone()
                .unwrap_or_else(|| format!("HEAD:{}", lane.path.display())),
            generation,
            origin: if ownership
                .as_ref()
                .is_some_and(|r| r.origin == worktrees::RefOrigin::Created)
            {
                RetirementOrigin::Created
            } else {
                RetirementOrigin::Adopted
            },
        };
        let incarnation = ownership
            .as_ref()
            .map(|r| r.recorded_at.as_micros().to_string())
            .unwrap_or_else(|| generation.to_string());
        let id = if let Some(id) = prior_id {
            id.to_owned()
        } else {
            retirement_id(&reference, &incarnation, &lane.path, lane.head)?
        };
        match context
            .project_state
            .mkdir(OsStr::new("retirements"), 0o700)
        {
            Ok(()) => {}
            Err(e) if e.is_exists() => {}
            Err(e) => return Err(e.into()),
        }
        let directory = context
            .project_state
            .open_verified_directory(OsStr::new("retirements"))?;
        let name = format!("{id}.json");
        let current = match directory.read_bounded(OsStr::new(&name), 64 * 1024) {
            Ok(raw) => Some(
                serde_json::from_slice::<RetirementRecord>(&raw)
                    .map_err(|_| WorktreeError::InvalidOutput)?,
            ),
            Err(e) if e.is_not_found() => None,
            Err(e) => return Err(e.into()),
        };
        if prior_id.is_some() && current.is_none() {
            return Err(WorktreeError::NotFound);
        }
        let decision_reason = match action {
            RetirementAction::Idle => "idle clean worktree; preserve branch".to_owned(),
            RetirementAction::Subsumed(evidence) => {
                serde_json::to_string(evidence).map_err(|_| WorktreeError::InvalidOutput)?
            }
        };
        let mut record = current.unwrap_or_else(|| RetirementRecord {
            schema_version: 2,
            id: id.clone(),
            reference: reference.clone(),
            expected_tip: lane.head.to_string(),
            decision: RetirementDecision {
                authority: "repository retention policy".into(),
                reason: decision_reason,
                action: match action {
                    RetirementAction::Idle => {
                        provenant_records::landing::RetirementAction::Preserve
                    }
                    RetirementAction::Subsumed(_) => {
                        provenant_records::landing::RetirementAction::Discard
                    }
                },
                decided_at: now.as_micros().to_string(),
            },
            recovery_reference: lane.branch.clone(),
            revision: 0,
            outcomes: Vec::new(),
        });
        if record.id != id
            || record.reference.repository != project.to_string()
            || record.reference.reference != reference.reference
            || record.expected_tip != lane.head.to_string()
            || (prior_id.is_none() && record.reference != reference)
        {
            return Err(WorktreeError::IdentityChanged);
        }
        let reason = match action {
            RetirementAction::Idle => "idle clean worktree; preserve branch".to_owned(),
            RetirementAction::Subsumed(evidence) => {
                serde_json::to_string(evidence).map_err(|_| WorktreeError::InvalidOutput)?
            }
        };
        let stored_action = match action {
            RetirementAction::Idle => provenant_records::landing::RetirementAction::Preserve,
            RetirementAction::Subsumed(_) => provenant_records::landing::RetirementAction::Discard,
        };
        if record.decision.reason != reason || record.decision.action != stored_action {
            record.decision.reason = reason;
            record.decision.action = stored_action;
            record.decision.decided_at = now.as_micros().to_string();
        }
        record.recovery_reference = match action {
            RetirementAction::Idle => lane.branch.clone(),
            RetirementAction::Subsumed(evidence) => match &evidence.source {
                LandingSource::RetainedRef { reference, .. }
                | LandingSource::IntegrationTarget { reference, .. } => Some(reference.clone()),
                _ => None,
            },
        };
        record.revision = record.revision.saturating_add(1);
        let step = if lane.primary {
            format!("branch:{}", lane.branch.as_deref().unwrap_or("HEAD"))
        } else {
            format!("worktree:{}", lane.path.display())
        };
        record.outcomes.retain(|s| s.step != step);
        record.outcomes.push(RetirementStepOutcome {
            step,
            state: if completed {
                RetirementStepState::Completed {
                    detail: Some("worktree removed; branch policy applied".into()),
                }
            } else {
                RetirementStepState::Pending
            },
        });
        if completed && matches!(action, RetirementAction::Subsumed(_)) {
            if let Some(branch) = &lane.branch {
                let reference = provenant_confinement::repo::GitRefName::new(branch)
                    .map_err(|_| WorktreeError::InvalidOutput)?;
                let state = match worktrees::ref_tip(context, &reference, &CancelToken::new()) {
                    Ok(None) => RetirementStepState::Completed {
                        detail: Some("exact-tip branch removed".into()),
                    },
                    Ok(Some(tip)) if tip == lane.head => RetirementStepState::Pending,
                    Ok(Some(_)) => RetirementStepState::Refused {
                        reason: "branch tip changed; new work retained".into(),
                    },
                    Err(error) => RetirementStepState::Unknown {
                        reason: error.to_string(),
                    },
                };
                let step = format!("branch:{branch}");
                record.outcomes.retain(|s| s.step != step);
                record.outcomes.push(RetirementStepOutcome { step, state });
            }
        }
        let bytes = serde_json::to_vec(&record).map_err(|_| WorktreeError::InvalidOutput)?;
        directory
            .atomic_write(
                OsStr::new(&name),
                ".retirement-",
                &bytes,
                PublishMode::Replace,
            )?
            .require_durable()
            .map_err(|_| WorktreeError::InvalidOutput)?;
        let host_context = self
            .context()
            .map_err(|detail| WorktreeError::UnknownOccupancy {
                path: lane.path.clone(),
                detail,
            })?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let host =
            HostStore::open(&self.roots.state, &call).map_err(|_| WorktreeError::UnknownWriter)?;
        let holder = Holder::Chair {
            chair: provenant_cli::fabric::current_chair(&host_context),
        };
        for subject in Self::subjects(lane) {
            if completed
                && subject.kind == SubjectKind::Branch
                && matches!(action, RetirementAction::Subsumed(_))
                && !record.outcomes.iter().any(|step| {
                    step.step == format!("branch:{}", subject.id)
                        && matches!(step.state, RetirementStepState::Completed { .. })
                })
            {
                continue;
            }
            if completed {
                if host
                    .custody_item(project, &subject, &call)
                    .map_err(|_| WorktreeError::UnknownWriter)?
                    .is_some_and(|r| r.state == CustodyState::Active)
                {
                    continue;
                }
            }

            if host
                .custody_item(project, &subject, &call)
                .map_err(|_| WorktreeError::UnknownWriter)?
                .is_none()
            {
                host.acquire_custody(
                    project,
                    &subject,
                    &holder,
                    if lane.is_unowned() {
                        Origin::External
                    } else {
                        Origin::Provenant
                    },
                    &call,
                )
                .map_err(|_| WorktreeError::UnknownWriter)?;
            }
            let state = match action {
                RetirementAction::Idle => CustodyState::Retired {
                    retirement_id: id.clone(),
                },
                RetirementAction::Subsumed(evidence)
                    if completed && evidence.predicate == LandingPredicate::Landed =>
                {
                    CustodyState::Landed {
                        evidence: evidence.clone(),
                    }
                }
                RetirementAction::Subsumed(_) => CustodyState::Retired {
                    retirement_id: id.clone(),
                },
            };
            host.change_custody_state(
                project,
                &subject,
                state,
                Some(host_context.identity.inbox.actor),
                &call,
            )
            .map_err(|_| WorktreeError::UnknownWriter)?;
        }
        Ok(id)
    }
    fn remote_guard(
        &self,
        context: &WorktreeContext<'_>,
        branch: &str,
        tip: GitOid,
        min_days: u64,
    ) -> Result<(), String> {
        self.remote_safe(branch, tip, min_days, context)
    }
    fn projection(
        &self,
        context: &WorktreeContext<'_>,
        id: &str,
        subject: Subject,
    ) -> Result<Option<RemovalRecord>, WorktreeError> {
        let directory = context
            .project_state
            .open_verified_directory(OsStr::new("retirements"))?;
        let raw = directory.read_bounded(OsStr::new(&format!("{id}.json")), 64 * 1024)?;
        let record: RetirementRecord =
            serde_json::from_slice(&raw).map_err(|_| WorktreeError::InvalidOutput)?;
        if record.id != id
            || record.reference.repository != context.binding.project_id().to_string()
        {
            return Err(WorktreeError::IdentityChanged);
        }
        if record.decision.action == provenant_records::landing::RetirementAction::Preserve {
            return Ok(None);
        }
        let step = format!(
            "{}:{}",
            if subject.kind == SubjectKind::Branch {
                "branch"
            } else {
                "worktree"
            },
            subject.id
        );
        if !record
            .outcomes
            .iter()
            .any(|s| s.step == step && matches!(s.state, RetirementStepState::Completed { .. }))
        {
            return Ok(None);
        }
        let evidence: LandedEvidence = serde_json::from_str(&record.decision.reason)
            .map_err(|_| WorktreeError::InvalidOutput)?;
        let tip = record
            .expected_tip
            .parse()
            .map_err(|_| WorktreeError::InvalidOutput)?;
        Ok(Some(RemovalRecord {
            project: context.binding.project_id(),
            subject,
            tip,
            outcome: RemovalOutcome::Removed { evidence },
        }))
    }
}

fn retirement_id(
    reference: &RetirementRef,
    incarnation: &str,
    path: &Path,
    tip: GitOid,
) -> Result<String, WorktreeError> {
    let encoded = serde_json::to_vec(reference).map_err(|_| WorktreeError::InvalidOutput)?;
    let mut hash = Sha256::new();
    hash.update(b"retirement-v2");
    let tip = tip.to_string();
    for bytes in [
        encoded.as_slice(),
        incarnation.as_bytes(),
        path.as_os_str().as_encoded_bytes(),
        tip.as_bytes(),
    ] {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok(provenant_core::hex(&hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retirement_identity_includes_ref_generation_and_incarnation() {
        let tip = GitOid::parse(&"ab".repeat(20)).unwrap();
        let reference = RetirementRef {
            repository: "fixture".into(),
            endpoint: RetirementEndpoint::Local,
            reference: "refs/heads/one".into(),
            generation: 1,
            origin: RetirementOrigin::Adopted,
        };
        let first = retirement_id(&reference, "first", Path::new("/fixture"), tip).unwrap();
        let mut changed = reference.clone();
        changed.reference = "refs/heads/two".into();
        assert_ne!(
            first,
            retirement_id(&changed, "first", Path::new("/fixture"), tip).unwrap()
        );
        changed = reference.clone();
        changed.generation += 1;
        assert_ne!(
            first,
            retirement_id(&changed, "first", Path::new("/fixture"), tip).unwrap()
        );
        assert_ne!(
            first,
            retirement_id(&reference, "recreated", Path::new("/fixture"), tip).unwrap()
        );
    }
}
