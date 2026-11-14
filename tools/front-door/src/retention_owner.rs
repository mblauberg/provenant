//! Application-owned custody and forge observations for retention decisions.
use provenant_confinement::worktrees::{
    self, ForgeObservation, WorktreeContext, WorktreeError, WorktreeInfo,
};
use provenant_core::{CancelToken, GitOid, Roots};
use provenant_fabric_store::{HostStore, Request};
use provenant_os::{clock::SystemClock, exec::Termination};
use provenant_records::{custody::*, landing::*};
use serde::Deserialize;
use std::{
    cell::RefCell,
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

    /// Reconcile durable pending steps through confinement's single lifecycle owner,
    /// then project the updated S2 records into the GC report.
    pub(crate) fn recover(
        &self,
        context: &WorktreeContext<'_>,
        port: &dyn worktrees::RetirementPort,
        cancel: &CancelToken,
    ) -> Result<Vec<RemovalRecord>, WorktreeError> {
        let records = worktrees::resume_pending_retirements(context, port, cancel)?;
        Ok(records
            .into_iter()
            .flat_map(|record| record.removals())
            .filter(|removal| {
                matches!(
                    removal.outcome,
                    provenant_records::custody::RemovalOutcome::Removed { .. }
                )
            })
            .collect())
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
impl worktrees::RetirementFeeds for Owner<'_> {
    fn authority(&self) -> Result<provenant_core::ActorId, String> {
        Ok(self.context()?.identity.inbox.actor)
    }

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
                    if matches!(record.holder, Holder::Chair { .. })
                        && record
                            .holder
                            .belongs_to(&provenant_cli::fabric::current_chair(&context))
                    {
                        continue;
                    }
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
    fn remote_guard(
        &self,
        context: &WorktreeContext<'_>,
        branch: &str,
        tip: GitOid,
        min_days: u64,
    ) -> Result<(), String> {
        self.remote_safe(branch, tip, min_days, context)
    }
}
