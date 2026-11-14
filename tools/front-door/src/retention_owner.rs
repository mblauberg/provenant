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
}
impl<'a> Owner<'a> {
    pub(crate) fn new(roots: &'a Roots, home: &'a Path, project: &'a Path) -> Self {
        Self {
            roots,
            home,
            project,
            pulls: RefCell::new(None),
        }
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
    pub(crate) fn branch(
        &self,
        context: &WorktreeContext<'_>,
        reference: &provenant_confinement::repo::GitRefName,
        tip: GitOid,
    ) -> WorktreeInfo {
        WorktreeInfo {
            warnings: Vec::new(),
            path: context.repo.root_path().into(),
            branch: Some(reference.as_str().into()),
            head: tip,
            dirty: worktrees::WorktreeCondition::Clean,
            owner: Ok(None),
            primary: true,
            occupancy: worktrees::Occupancy::Available,
        }
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
    fn integration_bases(&self) -> Vec<String> {
        self.pulls(false)
            .map(|pulls| {
                pulls
                    .into_iter()
                    .filter(|p| p.state == "open")
                    .flat_map(|p| {
                        [
                            format!("refs/heads/{}", p.base.name),
                            format!("refs/remotes/origin/{}", p.base.name),
                        ]
                    })
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
    ) -> Result<String, WorktreeError> {
        // This function is the sole replaceable S2 persistence seam. Reports
        // and custody point to this frozen RetirementRecord; no second record.
        let project = context.binding.project_id();
        let mut digest = Sha256::new();
        digest.update(project.to_string());
        digest.update(lane.path.as_os_str().as_encoded_bytes());
        digest.update(lane.head.to_string());
        let id = provenant_core::hex(&digest.finalize());
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
        let ledger = worktrees::RefLedger::for_context(context)?;
        let ownership = ledger.list()?.into_iter().find(|r| {
            r.endpoint == worktrees::RefEndpoint::Local
                && lane.branch.as_ref() == Some(&r.reference)
        });
        let decided_at = context
            .clock
            .wall_now()
            .map_err(|_| WorktreeError::InvalidOutput)?
            .as_micros()
            .to_string();
        let mut record = current.unwrap_or_else(|| RetirementRecord {
            schema_version: 2,
            id: id.clone(),
            reference: RetirementRef {
                repository: project.to_string(),
                endpoint: RetirementEndpoint::Local,
                reference: lane.branch.clone().unwrap_or_else(|| "HEAD".into()),
                generation: ownership.as_ref().map_or(1, |r| r.generation),
                origin: if ownership
                    .as_ref()
                    .is_some_and(|r| r.origin == worktrees::RefOrigin::Created)
                {
                    RetirementOrigin::Created
                } else {
                    RetirementOrigin::Adopted
                },
            },
            expected_tip: lane.head.to_string(),
            decision: RetirementDecision {
                authority: "repository retention policy".into(),
                reason: match action {
                    RetirementAction::Idle => "idle clean worktree; preserve branch".into(),
                    RetirementAction::Subsumed(evidence) => {
                        serde_json::to_string(evidence).expect("typed evidence serialises")
                    }
                },
                action: match action {
                    RetirementAction::Idle => {
                        provenant_records::landing::RetirementAction::Preserve
                    }
                    RetirementAction::Subsumed(_) => {
                        provenant_records::landing::RetirementAction::Discard
                    }
                },
                decided_at: context
                    .clock
                    .wall_now()
                    .map(|t| t.as_micros().to_string())
                    .unwrap_or_default(),
            },
            recovery_reference: lane.branch.clone(),
            revision: 0,
            outcomes: Vec::new(),
        });
        if record.expected_tip != lane.head.to_string() {
            return Err(WorktreeError::IdentityChanged);
        }
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
}
