//! Removal of the refs finished lanes leave behind (#3039).
//!
//! The candidates are the refs the shared ledger records as created by
//! Provenant: local branches, and remote branches at the endpoint a delivery
//! bound. A ref is removed once the shared landed check proves it landed, or
//! once it has stayed unlanded for `cleanup.unlanded_days`. Everything else is
//! retained: adopted or unobserved refs, a branch any worktree has checked
//! out, the integration ref itself and a ref that moved since it was checked.
//! Every deletion is leased on the tip just checked. Each artefact reports one
//! outcome; a failure is reported and never fails the caller.
use crate::gc_support::host_git_with_stderr;
use provenant_confinement::repo::GitRefName;
use provenant_confinement::worktrees::{
    self, Artefact, ArtefactOutcome, ArtefactState, IntegrationEdge, LandingTarget, RefDeletion,
    RefEndpoint, RefLedger, RefOrigin, RefRecord, WorktreeContext,
};
use provenant_core::{CancelToken, GitOid, WallTime};
use provenant_os::exec::Termination;
use std::time::Duration;

const REMOTE_READ: Duration = Duration::from_secs(30);
const REMOTE_WRITE: Duration = Duration::from_secs(60);

/// The policy one pass applies.
pub(crate) struct Sweep {
    pub(crate) unlanded_seconds: i64,
    pub(crate) now: WallTime,
    pub(crate) dry_run: bool,
}

impl Sweep {
    pub(crate) fn new(settings: &provenant_config::CleanupSettings, dry_run: bool) -> Option<Self> {
        use provenant_core::Clock;
        Some(Self {
            unlanded_seconds: settings.unlanded_seconds(),
            now: provenant_os::clock::SystemClock.wall_now().ok()?,
            dry_run,
        })
    }

    fn expired(&self, record: &RefRecord) -> bool {
        let age = self
            .now
            .as_micros()
            .saturating_sub(record.recorded_at.as_micros())
            / 1_000_000;
        age >= self.unlanded_seconds
    }
}

/// Why a ref may go, decided before any mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Landed,
    Expired,
    Keep(&'static str),
}

fn verdict(landed: Option<bool>, expired: bool) -> Verdict {
    match (landed, expired) {
        (Some(true), _) => Verdict::Landed,
        (_, true) => Verdict::Expired,
        (Some(false), false) => Verdict::Keep("not_landed"),
        (None, false) => Verdict::Keep("target_unavailable"),
    }
}

fn reason(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Landed => "landed",
        Verdict::Expired => "unlanded_expired",
        Verdict::Keep(reason) => reason,
    }
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

fn landed_on_any(
    context: &WorktreeContext<'_>,
    tip: GitOid,
    targets: &[LandingTarget],
    chain: &[IntegrationEdge],
    cancel: &CancelToken,
) -> Option<bool> {
    if targets.is_empty() {
        return None;
    }
    let mut observed = false;
    for target in targets {
        match worktrees::landing_in_repo(context, tip, target, chain, cancel) {
            Ok(landing) if landing.is_landed() => return Some(true),
            Ok(_) => observed = true,
            Err(_) => {}
        }
    }
    observed.then_some(false)
}

/// Remove one local branch Provenant created, when it has landed on one of
/// `targets` or has expired.
pub(crate) fn sweep_local(
    context: &WorktreeContext<'_>,
    ledger: &RefLedger,
    record: &RefRecord,
    targets: &[LandingTarget],
    sweep: &Sweep,
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
    sweep: &Sweep,
    cancel: &CancelToken,
) -> ArtefactOutcome {
    let name = record.reference.clone();
    let out = |state| ArtefactOutcome::new(Artefact::LocalBranch, name.clone(), state);
    if record.origin != RefOrigin::Created || record.endpoint != RefEndpoint::Local {
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
    match worktrees::branch_checked_out(context, &reference, cancel) {
        Ok(false) => {}
        Ok(true) => return out(ArtefactState::Retained).with_reason("checked_out"),
        Err(error) => return out(ArtefactState::Failed).with_reason(error.to_string()),
    }
    let verdict = verdict(
        landed_on_any(context, tip, targets, &[], cancel),
        sweep.expired(record),
    );
    if let Verdict::Keep(why) = verdict {
        return out(ArtefactState::Retained).with_reason(why).with_head(tip);
    }
    if sweep.dry_run {
        return out(ArtefactState::WouldRemove)
            .with_reason(reason(verdict))
            .with_head(tip);
    }
    match worktrees::delete_ref_leased(context, &reference, tip, cancel) {
        Ok(RefDeletion::Removed) => {
            let _ = ledger.forget(record, context.clock, cancel);
            out(ArtefactState::Removed)
                .with_reason(reason(verdict))
                .with_head(tip)
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
/// has landed on `target` (through ancestry or `chain`) or has expired.
pub(crate) fn sweep_remote(
    context: &WorktreeContext<'_>,
    ledger: &RefLedger,
    record: &RefRecord,
    target: Option<&LandingTarget>,
    chain: &[IntegrationEdge],
    sweep: &Sweep,
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
    sweep: &Sweep,
    cancel: &CancelToken,
) -> Vec<ArtefactOutcome> {
    let RefEndpoint::Remote { remote, push_url } = &record.endpoint else {
        return Vec::new();
    };
    let name = record.reference.clone();
    let out = |state| {
        ArtefactOutcome::new(Artefact::RemoteBranch, name.clone(), state).with_remote(remote)
    };
    if record.origin != RefOrigin::Created {
        return vec![out(ArtefactState::Retained).with_reason("not_created")];
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
            outcomes.extend(stale_tracking(context, remote, tracking, sweep, cancel));
        }
        return outcomes;
    };
    let landed = target.map(|target| {
        worktrees::landing_in_repo(context, tip, target, chain, cancel)
            .is_ok_and(|landing| landing.is_landed())
    });
    let verdict = verdict(landed, sweep.expired(record));
    if let Verdict::Keep(why) = verdict {
        return vec![out(ArtefactState::Retained).with_reason(why).with_head(tip)];
    }
    // An expired, unlanded ref is removed only while its commit stays here,
    // so the outcome's head can restore it.
    if verdict == Verdict::Expired && !worktrees::has_commit(context, tip, cancel).unwrap_or(false)
    {
        return vec![
            out(ArtefactState::Retained)
                .with_reason("head_not_local")
                .with_head(tip),
        ];
    }
    let mut outcomes = Vec::new();
    if sweep.dry_run {
        outcomes.push(
            out(ArtefactState::WouldRemove)
                .with_reason(reason(verdict))
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
    match delete_remote(context, push_url, &reference, tip) {
        Ok(()) => {
            let _ = ledger.forget(record, context.clock, cancel);
            outcomes.push(
                out(ArtefactState::Removed)
                    .with_reason(reason(verdict))
                    .with_head(tip),
            );
            if let Some(tracking) = &tracking {
                outcomes.extend(stale_tracking(context, remote, tracking, sweep, cancel));
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
    sweep: &Sweep,
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
    if sweep.dry_run {
        return Some(out(ArtefactState::WouldRemove).with_head(held));
    }
    let outcome = match worktrees::delete_ref_leased(context, tracking, held, cancel) {
        Ok(RefDeletion::Removed) => out(ArtefactState::Removed)
            .with_reason("remote_branch_gone")
            .with_head(held),
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
    let (_, branch) = fetched.0.strip_prefix("refs/remotes/")?.split_once('/')?;
    Some(LandingTarget {
        reference: Some(GitRefName::branch(branch).ok()?),
        tip: fetched.1,
    })
}

/// One GC pass over the ledger: every created ref, landed or expired, goes.
/// `upstream` is the default branch GC already fetched, when it did.
pub(crate) fn collect(
    context: &WorktreeContext<'_>,
    upstream: Option<LandingTarget>,
    sweep: &Sweep,
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
    let created = records
        .into_iter()
        .filter(|record| record.origin == RefOrigin::Created)
        .collect::<Vec<_>>();
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
    sweep: &Sweep,
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn landed_refs_go_unlanded_refs_stay_until_they_expire() {
        assert_eq!(verdict(Some(true), false), Verdict::Landed);
        assert_eq!(verdict(Some(true), true), Verdict::Landed);
        assert_eq!(verdict(Some(false), false), Verdict::Keep("not_landed"));
        assert_eq!(verdict(Some(false), true), Verdict::Expired);
        // A target that could not be read never proves landing, and an
        // unexpired ref is kept.
        assert_eq!(verdict(None, false), Verdict::Keep("target_unavailable"));
        assert_eq!(verdict(None, true), Verdict::Expired);
    }

    #[test]
    fn age_counts_from_the_ledger_record() {
        let record = |recorded: i64| RefRecord {
            schema_version: 1,
            repository: provenant_core::ProjectId::from_bytes([1; 16]),
            endpoint: RefEndpoint::Local,
            reference: "refs/heads/lane/x".into(),
            generation: 1,
            origin: RefOrigin::Created,
            tip: GitOid::parse(&"ab".repeat(20)).unwrap(),
            recorded_at: WallTime::from_unix_seconds(recorded).unwrap(),
            attempt_id: None,
            operation_id: None,
        };
        let sweep = Sweep {
            unlanded_seconds: 14 * 86_400,
            now: WallTime::from_unix_seconds(100 * 86_400).unwrap(),
            dry_run: true,
        };
        assert!(!sweep.expired(&record(100 * 86_400 - 13 * 86_400)));
        assert!(sweep.expired(&record(100 * 86_400 - 14 * 86_400)));
    }

    #[test]
    fn fetched_target_names_the_remote_default_branch() {
        let tip = GitOid::parse(&"cd".repeat(20)).unwrap();
        let target = fetched_target(&("refs/remotes/upstream/main".into(), tip)).unwrap();
        assert_eq!(target.reference, Some(GitRefName::branch("main").unwrap()));
        assert_eq!(target.tip, tip);
        assert!(fetched_target(&("refs/heads/main".into(), tip)).is_none());
    }
}
