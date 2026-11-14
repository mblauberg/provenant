//! Removal of the refs finished lanes leave behind (#3039).
//!
//! The candidates are the refs the shared ledger records as created by
//! Provenant: local branches, and remote branches at the endpoint a delivery
//! bound. A ref is removed only once the shared landed check proves it landed.
//! Unlanded refs are reported and retained regardless of age. Everything else is
//! retained: adopted or unobserved refs, a branch any worktree has checked
//! out, the integration ref itself and a ref that moved since it was checked.
//! Every deletion is leased on the tip just checked. Each artefact reports one
//! outcome; a failure is reported and never fails the caller.
use crate::gc_support::host_git_with_stderr;
use provenant_confinement::repo::GitRefName;
use provenant_confinement::worktrees::{
    self, Artefact, ArtefactOutcome, ArtefactState, IntegrationEdge, Landing, LandingTarget,
    RefDeletion, RefEndpoint, RefLedger, RefOrigin, RefRecord, Unproven, WorktreeContext,
};
use provenant_core::{CancelToken, GitOid};
use provenant_os::exec::Termination;
use provenant_records::custody::{
    LandedEvidence, LandingPredicate, LandingSource, RemovalOutcome, RemovalRecord,
    RemovalUncertainty, Subject, SubjectKind,
};
use std::time::Duration;

const REMOTE_READ: Duration = Duration::from_secs(30);
const REMOTE_WRITE: Duration = Duration::from_secs(60);

/// The policy one pass applies.
#[derive(Clone, Copy)]
pub(crate) struct Sweep<'a> {
    owner: Option<&'a dyn worktrees::RetirementOwner>,
    settings: Option<&'a provenant_config::ProjectGcSettings>,
    pub(crate) dry_run: bool,
}

impl<'a> Sweep<'a> {
    pub(crate) fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            owner: None,
            settings: None,
        }
    }
    pub(crate) fn with_retention(
        mut self,
        owner: &'a dyn worktrees::RetirementOwner,
        settings: &'a provenant_config::ProjectGcSettings,
    ) -> Self {
        self.owner = Some(owner);
        self.settings = Some(settings);
        self
    }
}

/// Why a ref may go, decided before any mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Proven(LandingPredicate),
    Keep(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LandingDecision {
    verdict: Verdict,
    removal: RemovalOutcome,
}

fn reason(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Proven(LandingPredicate::Landed) => "landed",
        Verdict::Proven(LandingPredicate::Subsumed) => "subsumed",
        Verdict::Keep(reason) => reason,
    }
}

fn landing_evidence(target: &LandingTarget) -> Option<LandedEvidence> {
    Some(LandedEvidence {
        predicate: LandingPredicate::Landed,
        source: LandingSource::IntegrationTarget {
            reference: target.reference.as_ref()?.as_str().to_owned(),
            commit: target.tip,
        },
    })
}

fn ref_removal(
    project: provenant_core::ProjectId,
    reference: &GitRefName,
    tip: GitOid,
    outcome: RemovalOutcome,
) -> RemovalRecord {
    RemovalRecord {
        project,
        subject: Subject::new(SubjectKind::Branch, reference.as_str()),
        tip,
        outcome,
    }
}

fn kept_decision(reason: &'static str, cause: RemovalUncertainty) -> LandingDecision {
    LandingDecision {
        verdict: Verdict::Keep(reason),
        removal: RemovalOutcome::Kept { cause },
    }
}

fn landed_on_any(
    context: &WorktreeContext<'_>,
    tip: GitOid,
    targets: &[LandingTarget],
    chain: &[IntegrationEdge],
    cancel: &CancelToken,
) -> LandingDecision {
    if targets.is_empty() {
        return kept_decision("target_unavailable", RemovalUncertainty::UnprovenLanding);
    }
    let mut observed_unproven = false;
    let mut missing_commit = false;
    for target in targets {
        match worktrees::landing_in_repo(context, tip, target, chain, cancel) {
            Ok(Landing::Landed(_)) => {
                if let Some(evidence) = landing_evidence(target) {
                    return LandingDecision {
                        verdict: Verdict::Proven(LandingPredicate::Landed),
                        removal: RemovalOutcome::Removed { evidence },
                    };
                }
            }
            Ok(Landing::Unproven(Unproven::MissingCommit { .. })) => {
                observed_unproven = true;
                missing_commit = true;
            }
            Ok(Landing::Unproven(_)) => observed_unproven = true,
            Err(_) => {}
        }
    }
    kept_decision(
        if observed_unproven {
            "not_landed"
        } else {
            "target_unavailable"
        },
        if missing_commit {
            RemovalUncertainty::MissingCommit
        } else {
            RemovalUncertainty::UnprovenLanding
        },
    )
}

fn warn(outcome: &ArtefactOutcome) {
    if outcome.outcome == ArtefactState::Failed {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "warning: could not remove {}: {}; it is retained",
            outcome.name,
            outcome.reason.as_deref().unwrap_or("unknown error")
        );
    }
}

/// Remove one local branch Provenant created, when it has landed on one of
/// `targets`, while its tip still matches the ledger.
pub(crate) fn sweep_local(
    context: &WorktreeContext<'_>,
    ledger: &RefLedger,
    record: &RefRecord,
    targets: &[LandingTarget],
    sweep: &Sweep<'_>,
    cancel: &CancelToken,
) -> ArtefactOutcome {
    let outcome = local_outcome(context, ledger, record, targets, sweep, cancel);
    warn(&outcome);
    outcome
}

fn local_outcome(
    context: &WorktreeContext<'_>,
    ledger: &RefLedger,
    record: &RefRecord,
    targets: &[LandingTarget],
    sweep: &Sweep<'_>,
    cancel: &CancelToken,
) -> ArtefactOutcome {
    let name = record.reference.clone();
    let out = |state| {
        ArtefactOutcome::new(Artefact::LocalBranch, name.clone(), state).with_class(
            if record.origin == RefOrigin::Created {
                worktrees::ArtefactClass::Managed
            } else {
                worktrees::ArtefactClass::Foreign
            },
        )
    };
    if record.endpoint != RefEndpoint::Local {
        return out(ArtefactState::Retained).with_reason("not_created");
    }
    let Ok(reference) = GitRefName::new(&record.reference) else {
        return out(ArtefactState::Retained).with_reason("invalid_ref");
    };
    if targets
        .iter()
        .any(|target| target.reference.as_ref() == Some(&reference))
    {
        return out(ArtefactState::Retained).with_reason("integration_ref");
    }
    let tip = match worktrees::ref_tip(context, &reference, cancel) {
        Ok(Some(tip)) => tip,
        Ok(None) => {
            if !sweep.dry_run {
                let _ = ledger.forget(record, context.clock, cancel);
            }
            return out(ArtefactState::Absent);
        }
        Err(error) => return out(ArtefactState::Failed).with_reason(error.to_string()),
    };
    if tip != record.tip {
        return out(ArtefactState::Retained)
            .with_reason("changed")
            .with_head(tip);
    }
    if let Some(settings) = sweep.settings {
        if record.origin == RefOrigin::Adopted
            && settings.foreign_worktrees == provenant_config::ForeignWorktrees::Keep
        {
            return out(ArtefactState::Retained)
                .with_reason("foreign cleanup disabled by repository configuration");
        }
        if settings
            .keep_paths
            .iter()
            .any(|p| worktrees::matches_pattern(reference.as_str(), p))
        {
            return out(ArtefactState::Retained).with_reason("excluded by gc.projects.keep_paths");
        }
    }
    match worktrees::branch_checked_out(context, &reference, cancel) {
        Ok(false) => {}
        Ok(true) => return out(ArtefactState::Retained).with_reason("checked_out"),
        Err(error) => return out(ArtefactState::Failed).with_reason(error.to_string()),
    }
    if let Some(owner) = sweep.owner {
        if let Err(error) = owner.custody(&worktrees::branch_candidate(context, &reference, tip)) {
            return out(ArtefactState::Retained).with_reason(error);
        }
    }
    let patterns = sweep
        .settings
        .map(|s| s.integration_branches.as_slice())
        .unwrap_or(&[]);
    let mut evidence_refs =
        match worktrees::retained_refs(context, Some(&reference), None, patterns, cancel) {
            Ok(refs) => refs,
            Err(error) => return out(ArtefactState::Retained).with_reason(error.to_string()),
        };
    for target in targets {
        if let Some(reference) = &target.reference {
            evidence_refs.push(worktrees::EvidenceRef {
                reference: reference.clone(),
                commit: target.tip,
                integration: true,
            });
        }
    }
    let decision = match worktrees::removal_evidence(context, tip, &evidence_refs, cancel) {
        Ok(worktrees::RemovalEvidence::Proven(evidence)) => LandingDecision {
            verdict: Verdict::Proven(evidence.predicate),
            removal: RemovalOutcome::Removed { evidence },
        },
        Ok(worktrees::RemovalEvidence::Unknown(cause)) => kept_decision("uncertain", cause),
        Ok(worktrees::RemovalEvidence::NotSubsumed) => {
            kept_decision("not_subsumed", RemovalUncertainty::UnprovenLanding)
        }
        Err(error) => return out(ArtefactState::Retained).with_reason(error.to_string()),
    };

    if let Verdict::Keep(why) = decision.verdict {
        return out(ArtefactState::Retained)
            .with_reason(why)
            .with_head(tip)
            .with_removal(ref_removal(
                context.binding.project_id(),
                &reference,
                tip,
                decision.removal,
            ));
    }
    if sweep.dry_run {
        return out(ArtefactState::WouldRemove)
            .with_reason(reason(decision.verdict))
            .with_head(tip);
    }
    let retirement_id = if let (Some(owner), RemovalOutcome::Removed { evidence }) =
        (sweep.owner, &decision.removal)
    {
        match owner.record(
            context,
            &worktrees::branch_candidate(context, &reference, tip),
            &worktrees::RetirementAction::Subsumed(evidence.clone()),
            false,
            None,
        ) {
            Ok(id) => Some(id),
            Err(error) => return out(ArtefactState::Retained).with_reason(error.to_string()),
        }
    } else {
        None
    };
    let eligible = || {
        if let Some(owner) = sweep.owner {
            owner
                .custody(&worktrees::branch_candidate(context, &reference, tip))
                .map_err(|detail| worktrees::WorktreeError::UnknownOccupancy {
                    path: context.repo.root_path().into(),
                    detail,
                })?;
        }
        if let RemovalOutcome::Removed { evidence } = &decision.removal {
            worktrees::validate_removal_evidence(context, tip, evidence, &evidence_refs, cancel)?;
        }
        Ok(())
    };
    match worktrees::delete_ref_leased_when(context, &reference, tip, cancel, &eligible) {
        Ok(RefDeletion::Removed) => {
            if let (Some(owner), RemovalOutcome::Removed { evidence }) =
                (sweep.owner, &decision.removal)
            {
                if let Err(error) = owner.record(
                    context,
                    &worktrees::branch_candidate(context, &reference, tip),
                    &worktrees::RetirementAction::Subsumed(evidence.clone()),
                    true,
                    retirement_id.as_deref(),
                ) {
                    return out(ArtefactState::Failed).with_reason(format!(
                        "branch removed; retirement completion pending: {error}"
                    ));
                }
            }
            let _ = ledger.forget(record, context.clock, cancel);
            let projected = if let (Some(owner), Some(id)) = (sweep.owner, retirement_id.as_deref())
            {
                match owner.projection(
                    context,
                    id,
                    Subject::new(SubjectKind::Branch, reference.as_str()),
                ) {
                    Ok(Some(record)) => record.outcome,
                    Ok(None) => {
                        return out(ArtefactState::Failed)
                            .with_reason("branch removed; retirement projection is incomplete");
                    }
                    Err(error) => return out(ArtefactState::Failed).with_reason(error.to_string()),
                }
            } else {
                decision.removal
            };
            out(ArtefactState::Removed)
                .with_reason(reason(decision.verdict))
                .with_head(tip)
                .with_removal(ref_removal(
                    context.binding.project_id(),
                    &reference,
                    tip,
                    projected,
                ))
        }
        Ok(RefDeletion::Absent) => {
            let _ = ledger.forget(record, context.clock, cancel);
            out(ArtefactState::Absent)
        }
        Ok(RefDeletion::Changed) => out(ArtefactState::Retained).with_reason("changed"),
        Ok(RefDeletion::CheckedOut) => out(ArtefactState::Retained).with_reason("checked_out"),
        Err(error) => out(ArtefactState::Failed).with_reason(error.to_string()),
    }
}

/// Remove one remote branch Provenant created, and its tracking ref, when it
/// has landed on `target` (through ancestry or `chain`).
pub(crate) fn sweep_remote(
    context: &WorktreeContext<'_>,
    ledger: &RefLedger,
    record: &RefRecord,
    target: Option<&LandingTarget>,
    chain: &[IntegrationEdge],
    sweep: &Sweep<'_>,
    cancel: &CancelToken,
) -> Vec<ArtefactOutcome> {
    let outcomes = remote_outcomes(context, ledger, record, target, chain, sweep, cancel);
    outcomes.iter().for_each(warn);
    outcomes
}

fn remote_outcomes(
    context: &WorktreeContext<'_>,
    ledger: &RefLedger,
    record: &RefRecord,
    target: Option<&LandingTarget>,
    chain: &[IntegrationEdge],
    sweep: &Sweep<'_>,
    cancel: &CancelToken,
) -> Vec<ArtefactOutcome> {
    let RefEndpoint::Remote { remote, push_url } = &record.endpoint else {
        return Vec::new();
    };
    let name = record.reference.clone();
    let out = |state| {
        ArtefactOutcome::new(Artefact::RemoteBranch, name.clone(), state)
            .with_remote(remote)
            .with_class(if record.origin == RefOrigin::Created {
                worktrees::ArtefactClass::Managed
            } else {
                worktrees::ArtefactClass::Foreign
            })
    };
    let policy = sweep
        .settings
        .map(|s| s.remote_branches)
        .unwrap_or_default();
    // Every bound endpoint here is pushed or explicitly held in the one ref
    // ledger. Unobserved remote refs are outside this deletion surface.
    if policy == provenant_config::RemoteBranches::Never {
        return vec![out(ArtefactState::Retained).with_reason("remote deletion disabled")];
    }
    let Ok(reference) = GitRefName::new(&record.reference) else {
        return vec![out(ArtefactState::Retained).with_reason("invalid_ref")];
    };
    if target.is_some_and(|target| target.reference.as_ref() == Some(&reference)) {
        return vec![out(ArtefactState::Retained).with_reason("integration_ref")];
    }
    let tracking = GitRefName::remote_tracking(format!("{remote}/{}", reference.short_name())).ok();
    let tip = match remote_tip(context, push_url, &reference) {
        Ok(tip) => tip,
        Err(error) => return vec![out(ArtefactState::Failed).with_reason(error)],
    };
    let Some(tip) = tip else {
        // Gone already, perhaps deleted on merge. A tracking ref left behind
        // is stale; drop it at the tip it holds.
        if !sweep.dry_run {
            let _ = ledger.forget(record, context.clock, cancel);
        }
        let mut outcomes = vec![out(ArtefactState::Absent)];
        if let Some(tracking) = &tracking {
            outcomes.extend(stale_tracking(
                context, remote, tracking, target, chain, sweep, cancel,
            ));
        }
        return outcomes;
    };
    if tip != record.tip {
        return vec![
            out(ArtefactState::Retained)
                .with_reason("changed")
                .with_head(tip),
        ];
    }
    let decision = landed_on_any(
        context,
        tip,
        target.map(std::slice::from_ref).unwrap_or(&[]),
        chain,
        cancel,
    );
    if let Verdict::Keep(why) = decision.verdict {
        return vec![
            out(ArtefactState::Retained)
                .with_reason(why)
                .with_head(tip)
                .with_removal(ref_removal(
                    context.binding.project_id(),
                    &reference,
                    tip,
                    decision.removal,
                )),
        ];
    }
    let mut outcomes = Vec::new();
    if sweep.dry_run {
        outcomes.push(
            out(ArtefactState::WouldRemove)
                .with_reason(reason(decision.verdict))
                .with_head(tip),
        );
        if let Some(tracking) = &tracking
            && let Ok(Some(held)) = worktrees::ref_tip(context, tracking, cancel)
        {
            outcomes.push(
                ArtefactOutcome::new(
                    Artefact::TrackingRef,
                    tracking.as_str(),
                    ArtefactState::WouldRemove,
                )
                .with_remote(remote)
                .with_head(held),
            );
        }
        return outcomes;
    }
    let Some(owner) = sweep.owner else {
        return vec![out(ArtefactState::Retained).with_reason("forge recheck unavailable")];
    };
    let days = sweep.settings.map_or(1, |s| s.remote_min_tip_age_days());
    let now = match context.clock.wall_now() {
        Ok(now) => now,
        Err(error) => return vec![out(ArtefactState::Retained).with_reason(error.to_string())],
    };
    if now
        .as_micros()
        .saturating_sub(record.recorded_at.as_micros())
        < i64::try_from(days.saturating_mul(86_400_000_000)).unwrap_or(i64::MAX)
    {
        return vec![
            out(ArtefactState::Retained)
                .with_reason("remote observation is younger than the minimum tip age"),
        ];
    }
    if let Err(error) = owner.remote_guard(context, reference.short_name(), tip, days) {
        return vec![
            out(ArtefactState::Retained)
                .with_reason(format!(
                    "{error}; gc.projects.{}.remote_min_tip_age_days={days} (source: {})",
                    context.binding.project_id(),
                    if sweep
                        .settings
                        .is_some_and(|s| s.remote_min_tip_age_days.is_some())
                    {
                        "configuration"
                    } else {
                        "compiled default"
                    }
                ))
                .with_head(tip)
                .with_removal(ref_removal(
                    context.binding.project_id(),
                    &reference,
                    tip,
                    RemovalOutcome::Kept {
                        cause: RemovalUncertainty::OfflineForge,
                    },
                )),
        ];
    }
    match delete_remote(context, push_url, &reference, tip) {
        Ok(()) => {
            let _ = ledger.forget(record, context.clock, cancel);
            outcomes.push(
                out(ArtefactState::Removed)
                    .with_reason(reason(decision.verdict))
                    .with_head(tip)
                    .with_removal(ref_removal(
                        context.binding.project_id(),
                        &reference,
                        tip,
                        decision.removal.clone(),
                    )),
            );
            if let Some(tracking) = &tracking {
                outcomes.extend(stale_tracking(
                    context, remote, tracking, target, chain, sweep, cancel,
                ));
            }
        }
        Err(error) => outcomes.push(out(ArtefactState::Failed).with_reason(error).with_head(tip)),
    }
    outcomes
}

/// Remove the tracking ref of a remote branch that is gone, at the tip it
/// holds now.
fn stale_tracking(
    context: &WorktreeContext<'_>,
    remote: &str,
    tracking: &GitRefName,
    target: Option<&LandingTarget>,
    chain: &[IntegrationEdge],
    sweep: &Sweep<'_>,
    cancel: &CancelToken,
) -> Option<ArtefactOutcome> {
    let out = |state| {
        ArtefactOutcome::new(Artefact::TrackingRef, tracking.as_str(), state).with_remote(remote)
    };
    let held = match worktrees::ref_tip(context, tracking, cancel) {
        Ok(Some(held)) => held,
        Ok(None) => return None,
        Err(error) => return Some(out(ArtefactState::Failed).with_reason(error.to_string())),
    };
    let decision = landed_on_any(
        context,
        held,
        target.map(std::slice::from_ref).unwrap_or(&[]),
        chain,
        cancel,
    );
    if let Verdict::Keep(why) = decision.verdict {
        return Some(
            out(ArtefactState::Retained)
                .with_reason(why)
                .with_head(held)
                .with_removal(ref_removal(
                    context.binding.project_id(),
                    tracking,
                    held,
                    decision.removal,
                )),
        );
    }
    if sweep.dry_run {
        return Some(out(ArtefactState::WouldRemove).with_head(held));
    }
    let outcome = match worktrees::delete_ref_leased(context, tracking, held, cancel) {
        Ok(RefDeletion::Removed) => out(ArtefactState::Removed)
            .with_reason("remote_branch_gone")
            .with_head(held)
            .with_removal(ref_removal(
                context.binding.project_id(),
                tracking,
                held,
                decision.removal,
            )),
        Ok(RefDeletion::Absent) => out(ArtefactState::Absent),
        Ok(_) => out(ArtefactState::Retained).with_reason("changed"),
        Err(error) => out(ArtefactState::Failed).with_reason(error.to_string()),
    };
    Some(outcome)
}

/// The remote's advertised tip for one full ref, or `None` when absent.
fn remote_tip(
    context: &WorktreeContext<'_>,
    push_url: &str,
    reference: &GitRefName,
) -> Result<Option<GitOid>, String> {
    let listed = host_git_with_stderr(
        context.repo.root_path(),
        context.executor,
        &["ls-remote", "--", push_url, reference.as_str()],
        REMOTE_READ,
        false,
    )
    .map_err(|error| format!("cannot read the remote branch: {error}"))?;
    if listed.termination != Termination::Exit(0) {
        return Err("cannot read the remote branch".into());
    }
    let text = std::str::from_utf8(&listed.stdout.bytes)
        .map_err(|_| "remote listing is not text".to_owned())?;
    let mut found = None;
    for line in text.lines() {
        let Some((oid, name)) = line.split_once('\t') else {
            return Err("remote listing is malformed".into());
        };
        if name != reference.as_str() {
            continue;
        }
        if found.is_some() {
            return Err("remote listing is ambiguous".into());
        }
        found = Some(GitOid::parse(oid).map_err(|_| "remote listing is malformed".to_owned())?);
    }
    Ok(found)
}

/// Delete a remote branch only while it is still at `expected`.
fn delete_remote(
    context: &WorktreeContext<'_>,
    push_url: &str,
    reference: &GitRefName,
    expected: GitOid,
) -> Result<(), String> {
    let lease = format!("--force-with-lease={}:{expected}", reference.as_str());
    let refspec = format!(":{}", reference.as_str());
    let pushed = host_git_with_stderr(
        context.repo.root_path(),
        context.executor,
        &[
            "push",
            "--porcelain",
            "--no-verify",
            &lease,
            "--",
            push_url,
            &refspec,
        ],
        REMOTE_WRITE,
        true,
    )
    .map_err(|error| format!("leased remote deletion did not complete: {error}"))?;
    if pushed.termination == Termination::Exit(0) {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&pushed.stderr.bytes);
    let detail = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("rejected")
        .trim()
        .chars()
        .take(200)
        .collect::<String>();
    Err(format!("remote refused the leased deletion: {detail}"))
}

/// Find and fetch the default branch the remote advertises, into the
/// tracking ref named for the bound remote.
fn remote_default(
    context: &WorktreeContext<'_>,
    remote: &str,
    push_url: &str,
    cancel: &CancelToken,
) -> Result<LandingTarget, String> {
    let run = |args: &[&str], timeout: Duration| {
        host_git_with_stderr(
            context.repo.root_path(),
            context.executor,
            args,
            timeout,
            false,
        )
        .map_err(|error| error.to_string())
    };
    let advertised = run(
        &["ls-remote", "--symref", "--", push_url, "HEAD"],
        REMOTE_READ,
    )?;
    if advertised.termination != Termination::Exit(0) {
        return Err("cannot observe the remote's default branch".into());
    }
    let text = std::str::from_utf8(&advertised.stdout.bytes)
        .map_err(|_| "invalid remote default branch".to_owned())?;
    let branch = text
        .lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        })
        .ok_or_else(|| "the remote has no default branch".to_owned())?;
    let reference =
        GitRefName::branch(branch).map_err(|_| "invalid remote default branch".to_owned())?;
    let tracking = GitRefName::remote_tracking(format!("{remote}/{branch}"))
        .map_err(|_| "invalid remote name".to_owned())?;
    let refspec = format!("+{}:{}", reference.as_str(), tracking.as_str());
    let fetched = run(
        &[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--refmap=",
            "--",
            push_url,
            &refspec,
        ],
        REMOTE_WRITE,
    )?;
    if fetched.termination != Termination::Exit(0) {
        return Err("cannot fetch the remote's default branch".into());
    }
    let tip = worktrees::ref_tip(context, &tracking, cancel)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "fetched default branch is missing".to_owned())?;
    Ok(LandingTarget {
        reference: Some(reference),
        tip,
    })
}

/// The landing target GC fetched from the repository's own remote, as
/// `refs/remotes/<remote>/<branch>` and its tip.
pub(crate) fn fetched_target(fetched: &(String, GitOid)) -> Option<LandingTarget> {
    fetched.0.strip_prefix("refs/remotes/")?.split_once('/')?;
    Some(LandingTarget {
        reference: Some(GitRefName::new(&fetched.0).ok()?),
        tip: fetched.1,
    })
}

/// One GC pass over the ledger: only proven landed, unchanged created refs go.
/// `upstream` is the default branch GC already fetched, when it did.
pub(crate) fn collect(
    context: &WorktreeContext<'_>,
    upstream: Option<LandingTarget>,
    sweep: &Sweep<'_>,
    cancel: &CancelToken,
) -> Vec<ArtefactOutcome> {
    let ledger = match RefLedger::for_context(context) {
        Ok(ledger) => ledger,
        Err(_) => return Vec::new(),
    };
    let records = match ledger.list() {
        Ok(records) => records,
        Err(error) => {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: could not read the ref ledger: {error}; its refs are retained"
            );
            return Vec::new();
        }
    };
    let mut created = records;
    if let Ok(refs) = worktrees::retained_refs(context, None, None, &[], cancel) {
        for retained in refs
            .into_iter()
            .filter(|r| r.reference.as_str().starts_with("refs/heads/"))
        {
            if !created.iter().any(|r| {
                r.endpoint == RefEndpoint::Local && r.reference == retained.reference.as_str()
            }) {
                if let Ok(now) = context.clock.wall_now() {
                    created.push(RefRecord {
                        schema_version: 2,
                        repository: context.binding.project_id(),
                        endpoint: RefEndpoint::Local,
                        reference: retained.reference.as_str().into(),
                        generation: 1,
                        origin: RefOrigin::Adopted,
                        tip: retained.commit,
                        recorded_at: now,
                        attempt_id: None,
                        operation_id: None,
                    });
                }
            }
        }
    }
    let first_foreign = worktrees::first_foreign_pass(context).unwrap_or(true)
        && created.iter().any(|record| {
            record.origin != RefOrigin::Created
                && (record.endpoint == RefEndpoint::Local
                    || sweep.settings.is_some_and(|settings| {
                        settings.remote_branches != provenant_config::RemoteBranches::Never
                    }))
        });
    let requested_apply = !sweep.dry_run;
    let effective = Sweep {
        dry_run: sweep.dry_run || first_foreign,
        ..*sweep
    };
    let sweep = &effective;
    // One default-branch fetch per bound endpoint.
    let mut endpoints: Vec<(RefEndpoint, Option<LandingTarget>)> = Vec::new();
    for record in &created {
        if let RefEndpoint::Remote { remote, push_url } = &record.endpoint
            && !endpoints
                .iter()
                .any(|(endpoint, _)| endpoint == &record.endpoint)
        {
            let target = remote_default(context, remote, push_url, cancel).ok();
            endpoints.push((record.endpoint.clone(), target));
        }
    }
    let mut outcomes = Vec::new();
    // Remote refs first, so a branch whose remote copy just went is judged
    // against the same fetched targets.
    for record in &created {
        if record.endpoint == RefEndpoint::Local {
            continue;
        }
        let target = endpoints
            .iter()
            .find(|(endpoint, _)| endpoint == &record.endpoint)
            .and_then(|(_, target)| target.as_ref());
        outcomes.extend(sweep_remote(
            context,
            &ledger,
            record,
            target,
            &[],
            sweep,
            cancel,
        ));
    }
    let mut targets = upstream.into_iter().collect::<Vec<_>>();
    for (_, target) in &endpoints {
        if let Some(target) = target
            && !targets.contains(target)
        {
            targets.push(target.clone());
        }
    }
    for record in &created {
        if record.endpoint == RefEndpoint::Local {
            outcomes.push(sweep_local(
                context, &ledger, record, &targets, sweep, cancel,
            ));
        }
    }
    if first_foreign && requested_apply {
        let _ = worktrees::acknowledge_foreign_preview(context);
    }
    outcomes
}

/// Settlement of an integrated lane: its own remote branch, when a delivery
/// created one at the bound endpoint, goes once the integration receipt
/// proves it landed. The local branch and worktree were settled by
/// `prune_owned`.
pub(crate) fn settle_integrated(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    receipt: &provenant_run::WriterDeliveryReceipt,
    sweep: &Sweep<'_>,
    cancel: &CancelToken,
) -> Vec<ArtefactOutcome> {
    let (Some(integration), Some(head)) = (receipt.integration.as_ref(), receipt.head) else {
        return Vec::new();
    };
    let Some(push_url) = integration.push_url.as_ref() else {
        return Vec::new();
    };
    let Ok(target) = GitRefName::new(&integration.target) else {
        return Vec::new();
    };
    let endpoint = RefEndpoint::Remote {
        remote: integration.remote.clone(),
        push_url: push_url.clone(),
    };
    let Ok(ledger) = RefLedger::for_context(context) else {
        return Vec::new();
    };
    let Ok(Some(record)) = ledger.load(&endpoint, branch) else {
        return Vec::new();
    };
    let root = LandingTarget {
        reference: Some(target.clone()),
        tip: integration.integrated,
    };
    let chain = [IntegrationEdge {
        source: branch.clone(),
        head,
        target,
        previous: integration.previous,
        integrated: integration.integrated,
    }];
    sweep_remote(
        context,
        &ledger,
        &record,
        Some(&root),
        &chain,
        sweep,
        cancel,
    )
}

#[cfg(test)]
// Custom harness=false drivers include this module without running its unit tests.
#[allow(dead_code, unused_imports)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    struct FreeWorkers;

    impl worktrees::WorkerObserver for FreeWorkers {
        fn observe_roots(
            &self,
            roots: &[std::path::PathBuf],
        ) -> std::collections::BTreeMap<std::path::PathBuf, worktrees::Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, worktrees::Occupancy::Available))
                .collect()
        }
    }

    struct CleanupFixture {
        _temp: tempfile::TempDir,
        home: std::path::PathBuf,
        project: std::path::PathBuf,
        remote: std::path::PathBuf,
        roots: provenant_core::Roots,
        base: GitOid,
        unlanded: GitOid,
        landed: GitOid,
    }

    impl CleanupFixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            use provenant_os::fixture::run_git;
            let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root))?;
            let home = temp.path().join("home");
            let project = temp.path().join("project");
            let remote = temp.path().join("remote.git");
            let state = temp.path().join("state");
            for path in [&home, &project, &remote] {
                std::fs::create_dir(path)?;
            }
            provenant_os::fixture::private_state_dir(&state)?;
            run_git(&remote, &["init", "--bare", "-q"])?;
            run_git(&project, &["init", "-q", "-b", "main"])?;
            run_git(&project, &["commit", "--allow-empty", "-qm", "base"])?;
            let head = || -> Result<GitOid, Box<dyn std::error::Error>> {
                Ok(GitOid::parse(
                    String::from_utf8(run_git(&project, &["rev-parse", "HEAD"])?)?.trim(),
                )?)
            };
            let base = head()?;
            run_git(&project, &["checkout", "-qb", "lane/old"])?;
            run_git(
                &project,
                &["commit", "--allow-empty", "-qm", "unlanded work"],
            )?;
            let unlanded = head()?;
            run_git(&project, &["checkout", "-q", "main"])?;
            run_git(
                &project,
                &["commit", "--allow-empty", "-qm", "human work landed"],
            )?;
            let landed = head()?;
            run_git(&project, &["branch", "lane/changed"])?;
            run_git(
                &project,
                &[
                    "remote",
                    "add",
                    "bound",
                    remote.to_str().ok_or("remote path")?,
                ],
            )?;
            run_git(
                &project,
                &["push", "bound", "main", "lane/old", "lane/changed"],
            )?;
            let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
            inputs.state_dir = Some(state);
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            Ok(Self {
                _temp: temp,
                home,
                project,
                remote,
                roots,
                base,
                unlanded,
                landed,
            })
        }

        fn with_context<T>(
            &self,
            action: impl FnOnce(
                &WorktreeContext<'_>,
                &CancelToken,
            ) -> Result<T, crate::worktrees::Error>,
        ) -> Result<T, crate::worktrees::Error> {
            crate::worktrees::with_context(
                &self.roots,
                &self.home,
                &self.project,
                Duration::from_secs(30),
                &FreeWorkers,
                action,
            )
        }

        fn endpoint(&self) -> RefEndpoint {
            RefEndpoint::Remote {
                remote: "bound".into(),
                push_url: self.remote.to_string_lossy().into_owned(),
            }
        }
    }

    fn record_old_ref(
        context: &WorktreeContext<'_>,
        ledger: &RefLedger,
        endpoint: RefEndpoint,
        branch: &str,
        tip: GitOid,
        cancel: &CancelToken,
    ) -> Result<RefRecord, worktrees::WorktreeError> {
        ledger.record(
            worktrees::RefObservation {
                endpoint,
                reference: GitRefName::branch(branch)
                    .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                origin: RefOrigin::Created,
                tip,
                // Far beyond the former 14-day expiry threshold.
                recorded_at: provenant_core::WallTime::from_unix_seconds(1).unwrap(),
                attempt_id: None,
                operation_id: None,
            },
            context.clock,
            cancel,
        )
    }

    struct FixtureRetention;
    impl worktrees::RetirementOwner for FixtureRetention {
        fn custody(&self, _: &worktrees::WorktreeInfo) -> Result<(), String> {
            Ok(())
        }
        fn forge(&self, _: &GitRefName) -> worktrees::ForgeObservation {
            worktrees::ForgeObservation::Absent
        }
        fn integration_bases(&self) -> Vec<(String, GitOid)> {
            Vec::new()
        }
        fn record(
            &self,
            _: &WorktreeContext<'_>,
            _: &worktrees::WorktreeInfo,
            _: &worktrees::RetirementAction,
            _: bool,
            _: Option<&str>,
        ) -> Result<String, worktrees::WorktreeError> {
            Err(worktrees::WorktreeError::InvalidRequest)
        }
        fn projection(
            &self,
            _: &WorktreeContext<'_>,
            _: &str,
            _: Subject,
        ) -> Result<Option<RemovalRecord>, worktrees::WorktreeError> {
            Ok(None)
        }
        fn remote_guard(
            &self,
            _: &WorktreeContext<'_>,
            _: &str,
            _: GitOid,
            _: u64,
        ) -> Result<(), String> {
            Ok(())
        }
    }
    fn remote_fixture_sweep() -> Sweep<'static> {
        static OWNER: FixtureRetention = FixtureRetention;
        static SETTINGS: std::sync::OnceLock<provenant_config::ProjectGcSettings> =
            std::sync::OnceLock::new();
        Sweep::new(false).with_retention(
            &OWNER,
            SETTINGS.get_or_init(|| provenant_config::ProjectGcSettings {
                remote_min_tip_age_days: Some(0),
                ..Default::default()
            }),
        )
    }

    #[test]
    fn ancient_unlanded_local_and_remote_refs_survive_automatic_cleanup()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = CleanupFixture::new()?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let target = LandingTarget {
                reference: Some(GitRefName::branch("main").unwrap()),
                tip: fixture.landed,
            };
            for endpoint in [RefEndpoint::Local, fixture.endpoint()] {
                let record = record_old_ref(
                    context,
                    &ledger,
                    endpoint.clone(),
                    "lane/old",
                    fixture.unlanded,
                    cancel,
                )?;
                let outcomes = if endpoint == RefEndpoint::Local {
                    vec![sweep_local(
                        context,
                        &ledger,
                        &record,
                        std::slice::from_ref(&target),
                        &Sweep::new(false),
                        cancel,
                    )]
                } else {
                    sweep_remote(
                        context,
                        &ledger,
                        &record,
                        Some(&target),
                        &[],
                        &remote_fixture_sweep(),
                        cancel,
                    )
                };
                assert_eq!(outcomes.len(), 1);
                assert_eq!(outcomes[0].outcome, ArtefactState::Retained);
                assert_eq!(outcomes[0].reason.as_deref(), Some("not_landed"));
                assert_eq!(outcomes[0].head, Some(fixture.unlanded));
                assert_eq!(
                    outcomes[0].removal.as_ref().map(|record| &record.outcome),
                    Some(&RemovalOutcome::Kept {
                        cause: RemovalUncertainty::UnprovenLanding,
                    })
                );
                assert_eq!(
                    ledger.load(&endpoint, &GitRefName::branch("lane/old").unwrap())?,
                    Some(record)
                );
            }
            assert_eq!(
                worktrees::ref_tip(context, &GitRefName::branch("lane/old").unwrap(), cancel)?,
                Some(fixture.unlanded)
            );
            assert_eq!(
                remote_tip(
                    context,
                    fixture.remote.to_str().unwrap(),
                    &GitRefName::branch("lane/old").unwrap()
                )
                .unwrap(),
                Some(fixture.unlanded)
            );
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn human_changed_local_and_remote_tips_survive_even_when_landed()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = CleanupFixture::new()?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let target = LandingTarget {
                reference: Some(GitRefName::branch("main").unwrap()),
                tip: fixture.landed,
            };
            for endpoint in [RefEndpoint::Local, fixture.endpoint()] {
                let record = record_old_ref(
                    context,
                    &ledger,
                    endpoint.clone(),
                    "lane/changed",
                    fixture.base,
                    cancel,
                )?;
                let outcomes = if endpoint == RefEndpoint::Local {
                    vec![sweep_local(
                        context,
                        &ledger,
                        &record,
                        std::slice::from_ref(&target),
                        &Sweep::new(false),
                        cancel,
                    )]
                } else {
                    sweep_remote(
                        context,
                        &ledger,
                        &record,
                        Some(&target),
                        &[],
                        &remote_fixture_sweep(),
                        cancel,
                    )
                };
                assert_eq!(outcomes.len(), 1);
                assert_eq!(outcomes[0].outcome, ArtefactState::Retained);
                assert_eq!(outcomes[0].reason.as_deref(), Some("changed"));
                assert_eq!(outcomes[0].head, Some(fixture.landed));
                assert_eq!(
                    ledger.load(&endpoint, &GitRefName::branch("lane/changed").unwrap())?,
                    Some(record)
                );
            }
            assert_eq!(
                worktrees::ref_tip(
                    context,
                    &GitRefName::branch("lane/changed").unwrap(),
                    cancel
                )?,
                Some(fixture.landed)
            );
            assert_eq!(
                remote_tip(
                    context,
                    fixture.remote.to_str().unwrap(),
                    &GitRefName::branch("lane/changed").unwrap()
                )
                .unwrap(),
                Some(fixture.landed)
            );
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn removed_ref_records_the_exact_landing_target() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = CleanupFixture::new()?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let record = record_old_ref(
                context,
                &ledger,
                RefEndpoint::Local,
                "lane/changed",
                fixture.base,
                cancel,
            )?;
            let target = LandingTarget {
                reference: Some(GitRefName::branch("main").unwrap()),
                tip: fixture.landed,
            };
            let outcome = sweep_local(
                context,
                &ledger,
                &record,
                std::slice::from_ref(&target),
                &Sweep::new(false),
                cancel,
            );
            assert_eq!(outcome.outcome, ArtefactState::Removed);
            assert_eq!(
                outcome.removal,
                Some(RemovalRecord {
                    project: context.binding.project_id(),
                    subject: Subject::new(SubjectKind::Branch, "refs/heads/lane/changed"),
                    tip: fixture.base,
                    outcome: RemovalOutcome::Removed {
                        evidence: LandedEvidence {
                            predicate: LandingPredicate::Landed,
                            source: LandingSource::IntegrationTarget {
                                reference: "refs/heads/main".into(),
                                commit: fixture.landed,
                            },
                        },
                    },
                })
            );
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn stale_tracking_ref_is_kept_when_the_gone_remote_tip_is_unlanded()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = CleanupFixture::new()?;
        provenant_os::fixture::run_git(&fixture.project, &["fetch", "bound"])?;
        provenant_os::fixture::run_git(
            &fixture.remote,
            &["update-ref", "-d", "refs/heads/lane/old"],
        )?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let record = record_old_ref(
                context,
                &ledger,
                fixture.endpoint(),
                "lane/old",
                fixture.unlanded,
                cancel,
            )?;
            let target = LandingTarget {
                reference: Some(GitRefName::branch("main").unwrap()),
                tip: fixture.landed,
            };
            let tracking = GitRefName::remote_tracking("bound/lane/old").unwrap();
            let outcomes = sweep_remote(
                context,
                &ledger,
                &record,
                Some(&target),
                &[],
                &remote_fixture_sweep(),
                cancel,
            );
            let remote = outcomes
                .iter()
                .find(|outcome| outcome.artefact == Artefact::RemoteBranch)
                .unwrap();
            assert_eq!(remote.outcome, ArtefactState::Absent);
            let tracking_outcome = outcomes
                .iter()
                .find(|outcome| outcome.artefact == Artefact::TrackingRef)
                .unwrap();
            assert_eq!(tracking_outcome.outcome, ArtefactState::Retained);
            assert_eq!(tracking_outcome.head, Some(fixture.unlanded));
            let kept = tracking_outcome.removal.as_ref().unwrap();
            assert_eq!(kept.project, context.binding.project_id());
            assert_eq!(
                kept.outcome,
                RemovalOutcome::Kept {
                    cause: RemovalUncertainty::UnprovenLanding,
                }
            );
            assert_eq!(
                worktrees::ref_tip(context, &tracking, cancel)?,
                Some(fixture.unlanded)
            );
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn unknown_landing_target_and_unreachable_remote_preserve_ancient_work()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = CleanupFixture::new()?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let record = record_old_ref(
                context,
                &ledger,
                fixture.endpoint(),
                "lane/old",
                fixture.unlanded,
                cancel,
            )?;
            let outcomes = sweep_remote(
                context,
                &ledger,
                &record,
                None,
                &[],
                &remote_fixture_sweep(),
                cancel,
            );
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].outcome, ArtefactState::Retained);
            assert_eq!(outcomes[0].reason.as_deref(), Some("target_unavailable"));
            assert_eq!(
                remote_tip(
                    context,
                    fixture.remote.to_str().unwrap(),
                    &GitRefName::branch("lane/old").unwrap()
                )
                .unwrap(),
                Some(fixture.unlanded)
            );
            Ok(())
        })?;
        let offline = fixture.remote.with_extension("offline");
        std::fs::rename(&fixture.remote, &offline)?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let record = ledger
                .load(
                    &fixture.endpoint(),
                    &GitRefName::branch("lane/old").unwrap(),
                )?
                .unwrap();
            let outcomes = sweep_remote(
                context,
                &ledger,
                &record,
                None,
                &[],
                &remote_fixture_sweep(),
                cancel,
            );
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].outcome, ArtefactState::Failed);
            assert_eq!(
                ledger.load(
                    &fixture.endpoint(),
                    &GitRefName::branch("lane/old").unwrap()
                )?,
                Some(record)
            );
            assert_eq!(
                worktrees::ref_tip(context, &GitRefName::branch("lane/old").unwrap(), cancel)?,
                Some(fixture.unlanded)
            );
            Ok(())
        })?;
        std::fs::rename(&offline, &fixture.remote)?;
        let head =
            provenant_os::fixture::run_git(&fixture.remote, &["rev-parse", "refs/heads/lane/old"])?;
        assert_eq!(
            String::from_utf8(head)?.trim(),
            fixture.unlanded.to_string()
        );
        Ok(())
    }

    #[test]
    fn unproven_ref_decisions_keep_typed_causes() {
        let missing = kept_decision("not_landed", RemovalUncertainty::MissingCommit);
        assert_eq!(missing.verdict, Verdict::Keep("not_landed"));
        assert_eq!(
            missing.removal,
            RemovalOutcome::Kept {
                cause: RemovalUncertainty::MissingCommit
            }
        );
        let unknown = kept_decision("target_unavailable", RemovalUncertainty::UnprovenLanding);
        assert_eq!(unknown.verdict, Verdict::Keep("target_unavailable"));
        assert_eq!(
            unknown.removal,
            RemovalOutcome::Kept {
                cause: RemovalUncertainty::UnprovenLanding
            }
        );
    }

    #[test]
    fn fetched_target_names_the_remote_default_branch() {
        let tip = GitOid::parse(&"cd".repeat(20)).unwrap();
        let target = fetched_target(&("refs/remotes/upstream/main".into(), tip)).unwrap();
        assert_eq!(
            target.reference.as_ref().map(|r| r.as_str()),
            Some("refs/remotes/upstream/main")
        );
        assert_eq!(target.tip, tip);
        assert!(fetched_target(&("refs/heads/main".into(), tip)).is_none());
    }
}
