//! Advisory diagnosis shares admission's inventory, custody and Git probes.
use super::*;
use provenant_records::diagnostic::{Destination, safe_text, shell_path};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeDiagnosisState {
    Clean,
    Dirty,
    StaleRegistration,
    LiveWriter,
    Unregistered,
    Detached,
    GitOperationInProgress,
    Observed,
    Unknown,
}

/// A diagnosis grants no custody and never performs the suggested repair.
#[derive(Clone, Debug, Serialize)]
pub struct WorktreeDiagnosis {
    pub path: StructuredPathWire,
    pub state: WorktreeDiagnosisState,
    pub reason: String,
    pub repair: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<AttemptId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<StructuredPathWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
}

/// Inspect one named path, including registrations whose checkout is missing.
/// A lease is authoritative; a process observation remains advisory. Unknown
/// custody never suggests deleting a fence or taking over another writer.
pub fn doctor(
    context: &WorktreeContext<'_>,
    path: &Path,
    cancel: &CancelToken,
) -> Result<WorktreeDiagnosis, WorktreeError> {
    validate_binding(context)?;
    let requested = if path.is_absolute() {
        path.to_path_buf()
    } else {
        context.repo.root_path().join(path)
    };
    let path = match canonicalize(&requested) {
        Ok(path) => path,
        Err(error) if error.is_not_found() => crate::transport::normalise(&requested),
        Err(error) => return Err(error.into()),
    };
    let entries = {
        let _lock = repository_lock(context, LockMode::Shared, cancel)?;
        context.repo.revalidate()?;
        context.facts.read_inventory(context, cancel)?
    };
    let quoted = shell_path(&path, Destination::Local);
    let retry = quoted.as_ref().map_or_else(
        || "provenant worktree doctor PATH".to_owned(),
        |path| format!("provenant worktree doctor {path}"),
    );
    let mut report = WorktreeDiagnosis {
        path: StructuredPathWire::from_path(&path)?,
        state: WorktreeDiagnosisState::Unregistered,
        reason: "The path is not a registered checkout in this repository.".into(),
        repair: "provenant worktree create BRANCH".into(),
        holder: None,
        scope: None,
        evidence: None,
        source: None,
    };
    let Some(entry) = entries.iter().find(|entry| {
        !entry.bare
            && (entry.path == path || canonicalize(&entry.path).ok().as_ref() == Some(&path))
    }) else {
        return Ok(report);
    };
    let custody = disk_writer_state(context.project_state, &path);
    if (entry.prunable || !path.exists()) && custody.is_available() {
        report.state = WorktreeDiagnosisState::StaleRegistration;
        report.reason = "Git retains a registration for a missing checkout.".into();
        report.repair = shell_path(context.repo.root_path(), Destination::Local).map_or_else(
            || "git worktree prune".into(),
            |root| format!("git -C {root} worktree prune"),
        );
        return Ok(report);
    }
    let occupancy = match custody {
        Occupancy::Available if entry.locked => {
            Occupancy::custody_unknown(path.clone(), "Git marks the worktree locked")
        }
        Occupancy::Available => {
            let observed = context
                .workers
                .observe_roots(std::slice::from_ref(&path))
                .remove(&path)
                .unwrap_or_else(|| {
                    Occupancy::census_unknown(
                        path.clone(),
                        context.workers.incomplete_census_detail(),
                    )
                });
            combine_occupancy(&path, Occupancy::Available, observed)
        }
        other => other,
    };
    if let Occupancy::Held = occupancy {
        report.state = WorktreeDiagnosisState::LiveWriter;
        report.holder = writer_holder(context.project_state, &path);
        report.reason = report.holder.map_or_else(
            || "A writer holds the worktree lease.".into(),
            |holder| format!("Attempt {holder} holds the worktree lease."),
        );
        report.repair = "provenant status --wait".into();
        return Ok(report);
    }
    if let Occupancy::Unknown {
        scope,
        evidence,
        source,
    } = occupancy
    {
        report.state = WorktreeDiagnosisState::Unknown;
        report.reason =
            "Worktree occupancy could not be decided; inspect the scoped evidence before retrying."
                .into();
        report.scope = Some(StructuredPathWire::from_path(&scope)?);
        report.evidence = Some(safe_text(&evidence));
        report.source = Some(match source {
            OccupancyUnknownSource::Census => "census",
            OccupancyUnknownSource::Custody => "custody",
        });
        report.repair = retry;
        return Ok(report);
    }
    let verified = if path == context.repo.root_path() {
        None
    } else {
        match verify_probed_worktree(context, &path, cancel) {
            Ok(tree) => Some(tree),
            Err(error) => {
                report.state = WorktreeDiagnosisState::Unknown;
                report.reason = "The worktree registration could not be verified.".into();
                report.scope = Some(StructuredPathWire::from_path(&path)?);
                report.evidence = Some(safe_text(&error.to_string()));
                report.source = Some("custody");
                report.repair = retry;
                return Ok(report);
            }
        }
    };
    if let Some(tree) = verified.as_ref()
        && let Err(error) = load_owner(tree)
    {
        report.state = WorktreeDiagnosisState::Unknown;
        report.reason = "The worktree owner record could not be read.".into();
        report.scope = Some(StructuredPathWire::from_path(&path)?);
        report.evidence = Some(safe_text(&error.to_string()));
        report.source = Some("custody");
        report.repair = retry;
        return Ok(report);
    }
    let repo = verified.as_ref().map_or(context.repo, |tree| tree.repo());
    let git = quoted.map_or_else(|| "git -C PATH".into(), |path| format!("git -C {path}"));
    let condition = worktree_dirty_at(context, repo, cancel)?;
    let (state, reason, repair) = match condition {
        WorktreeCondition::Dirty => (
            WorktreeDiagnosisState::Dirty,
            "The checkout has uncommitted changes; preserve them before starting a writer.",
            format!("{git} stash push --include-untracked"),
        ),
        WorktreeCondition::GitOperationInProgress => (
            WorktreeDiagnosisState::GitOperationInProgress,
            "A Git operation is in progress; inspect its state and finish it before retrying.",
            format!("{git} status"),
        ),
        WorktreeCondition::Unknown => (
            WorktreeDiagnosisState::Unknown,
            "Checkout cleanliness could not be verified.",
            format!("{git} status"),
        ),
        _ if entry.branch.is_none() => (
            WorktreeDiagnosisState::Detached,
            "HEAD is detached; create a branch before starting a writer.",
            format!("{git} switch -c BRANCH"),
        ),
        _ if occupancy.is_observed() => (
            WorktreeDiagnosisState::Observed,
            "A process was observed in the checkout; writer custody is free and admission checks the Git boundary.",
            retry,
        ),
        _ => (
            WorktreeDiagnosisState::Clean,
            "The checkout is clean and writer custody is free; admission rechecks custody and the Git boundary.",
            retry,
        ),
    };
    report.state = state;
    report.reason = reason.into();
    report.repair = repair;
    if state == WorktreeDiagnosisState::Unknown {
        report.scope = Some(StructuredPathWire::from_path(&path)?);
        report.evidence = Some(report.reason.clone());
        report.source = Some("custody");
    }
    Ok(report)
}
