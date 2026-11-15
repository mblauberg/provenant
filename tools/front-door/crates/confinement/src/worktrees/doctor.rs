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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeRepairKind {
    RetryDiagnosis,
    CreateWorktree,
    PruneRegistration,
    WaitForWriter,
    PreserveChanges,
    InspectGit,
    CreateBranch,
}

impl WorktreeRepairKind {
    pub const ALL: &'static [Self] = &[
        Self::RetryDiagnosis,
        Self::CreateWorktree,
        Self::PruneRegistration,
        Self::WaitForWriter,
        Self::PreserveChanges,
        Self::InspectGit,
        Self::CreateBranch,
    ];
}

/// A diagnosis grants no custody and never performs the suggested repair.
#[derive(Clone, Debug, Serialize)]
pub struct WorktreeDiagnosis {
    pub path: StructuredPathWire,
    pub state: WorktreeDiagnosisState,
    pub reason: String,
    pub repair_kind: WorktreeRepairKind,
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

impl WorktreeDiagnosis {
    pub fn repair_kind(&self) -> WorktreeRepairKind {
        self.repair_kind
    }

    fn set_state(
        &mut self,
        state: WorktreeDiagnosisState,
        repair_kind: WorktreeRepairKind,
        repository: &Path,
        path: &Path,
    ) {
        self.state = state;
        self.repair_kind = repair_kind;
        self.repair = repair_kind.render(repository, path);
    }
}

impl WorktreeRepairKind {
    pub(super) fn render(self, repository: &Path, path: &Path) -> String {
        match self {
            Self::RetryDiagnosis => shell_path(path, Destination::Local).map_or_else(
                || "provenant worktree doctor PATH".into(),
                |path| format!("provenant worktree doctor {path}"),
            ),
            Self::CreateWorktree => "provenant worktree create BRANCH".into(),
            Self::PruneRegistration => shell_path(repository, Destination::Local).map_or_else(
                || "git worktree prune".into(),
                |root| format!("git -C {root} worktree prune"),
            ),
            Self::WaitForWriter => "provenant status --wait".into(),
            Self::PreserveChanges => shell_path(path, Destination::Local).map_or_else(
                || "git -C PATH stash push --include-untracked".into(),
                |path| format!("git -C {path} stash push --include-untracked"),
            ),
            Self::InspectGit => shell_path(path, Destination::Local).map_or_else(
                || "git -C PATH status".into(),
                |path| format!("git -C {path} status"),
            ),
            Self::CreateBranch => shell_path(path, Destination::Local).map_or_else(
                || "git -C PATH switch -c BRANCH".into(),
                |path| format!("git -C {path} switch -c BRANCH"),
            ),
        }
    }
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
    let mut report = WorktreeDiagnosis {
        path: StructuredPathWire::from_path(&path)?,
        state: WorktreeDiagnosisState::Unregistered,
        reason: "The path is not a registered checkout in this repository.".into(),
        repair_kind: WorktreeRepairKind::CreateWorktree,
        repair: String::new(),
        holder: None,
        scope: None,
        evidence: None,
        source: None,
    };
    report.set_state(
        WorktreeDiagnosisState::Unregistered,
        WorktreeRepairKind::CreateWorktree,
        context.repo.root_path(),
        &path,
    );
    let Some(entry) = entries.iter().find(|entry| {
        !entry.bare
            && (entry.path == path || canonicalize(&entry.path).ok().as_ref() == Some(&path))
    }) else {
        return Ok(report);
    };
    let custody = disk_writer_state(context.project_state, &path);
    if (entry.prunable || !path.exists()) && custody.is_available() {
        report.set_state(
            WorktreeDiagnosisState::StaleRegistration,
            WorktreeRepairKind::PruneRegistration,
            context.repo.root_path(),
            &path,
        );
        report.reason = "Git retains a registration for a missing checkout.".into();
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
        report.set_state(
            WorktreeDiagnosisState::LiveWriter,
            WorktreeRepairKind::WaitForWriter,
            context.repo.root_path(),
            &path,
        );
        report.holder = writer_holder(context.project_state, &path);
        report.reason = report.holder.map_or_else(
            || "A writer holds the worktree lease.".into(),
            |holder| format!("Attempt {holder} holds the worktree lease."),
        );
        return Ok(report);
    }
    if let Occupancy::Unknown {
        scope,
        evidence,
        source,
    } = occupancy
    {
        report.set_state(
            WorktreeDiagnosisState::Unknown,
            WorktreeRepairKind::RetryDiagnosis,
            context.repo.root_path(),
            &path,
        );
        report.reason =
            "Worktree occupancy could not be decided; inspect the scoped evidence before retrying."
                .into();
        report.scope = Some(StructuredPathWire::from_path(&scope)?);
        report.evidence = Some(safe_text(&evidence));
        report.source = Some(match source {
            OccupancyUnknownSource::Census => "census",
            OccupancyUnknownSource::Custody => "custody",
        });
        return Ok(report);
    }
    let verified = if path == context.repo.root_path() {
        None
    } else {
        match verify_probed_worktree(context, &path, cancel) {
            Ok(tree) => Some(tree),
            Err(error) => {
                report.set_state(
                    WorktreeDiagnosisState::Unknown,
                    WorktreeRepairKind::RetryDiagnosis,
                    context.repo.root_path(),
                    &path,
                );
                report.reason = "The worktree registration could not be verified.".into();
                report.scope = Some(StructuredPathWire::from_path(&path)?);
                report.evidence = Some(safe_text(&error.to_string()));
                report.source = Some("custody");
                return Ok(report);
            }
        }
    };
    if let Some(tree) = verified.as_ref()
        && let Err(error) = load_owner(tree)
    {
        report.set_state(
            WorktreeDiagnosisState::Unknown,
            WorktreeRepairKind::RetryDiagnosis,
            context.repo.root_path(),
            &path,
        );
        report.reason = "The worktree owner record could not be read.".into();
        report.scope = Some(StructuredPathWire::from_path(&path)?);
        report.evidence = Some(safe_text(&error.to_string()));
        report.source = Some("custody");
        return Ok(report);
    }
    let repo = verified.as_ref().map_or(context.repo, |tree| tree.repo());
    let condition = worktree_dirty_at(context, repo, cancel)?;
    let (state, reason, repair_kind) = match condition {
        WorktreeCondition::Dirty => (
            WorktreeDiagnosisState::Dirty,
            "The checkout has uncommitted changes; preserve them before starting a writer.",
            WorktreeRepairKind::PreserveChanges,
        ),
        WorktreeCondition::GitOperationInProgress => (
            WorktreeDiagnosisState::GitOperationInProgress,
            "A Git operation is in progress; inspect its state and finish it before retrying.",
            WorktreeRepairKind::InspectGit,
        ),
        WorktreeCondition::Unknown => (
            WorktreeDiagnosisState::Unknown,
            "Checkout cleanliness could not be verified.",
            WorktreeRepairKind::InspectGit,
        ),
        _ if entry.branch.is_none() => (
            WorktreeDiagnosisState::Detached,
            "HEAD is detached; create a branch before starting a writer.",
            WorktreeRepairKind::CreateBranch,
        ),
        _ if occupancy.is_observed() => (
            WorktreeDiagnosisState::Observed,
            "A process was observed in the checkout; writer custody is free and admission checks the Git boundary.",
            WorktreeRepairKind::RetryDiagnosis,
        ),
        _ => (
            WorktreeDiagnosisState::Clean,
            "The checkout is clean and writer custody is free; admission rechecks custody and the Git boundary.",
            WorktreeRepairKind::RetryDiagnosis,
        ),
    };
    report.set_state(state, repair_kind, context.repo.root_path(), &path);
    report.reason = reason.into();
    if state == WorktreeDiagnosisState::Unknown {
        report.scope = Some(StructuredPathWire::from_path(&path)?);
        report.evidence = Some(report.reason.clone());
        report.source = Some("custody");
    }
    Ok(report)
}
