use super::*;

/// Remove the empty generated staging directory under attempt exclusion.
/// Provider state/history and unclassified contents remain retained.
pub(crate) fn settle_launch_staging(attempt: &Directory) -> Result<()> {
    let name = OsStr::new("scratch");
    let scratch = match attempt.open_verified_directory(name) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if scratch.entries()?.next().transpose()?.is_none() {
        attempt.rmdir(name)?;
        attempt.sync()?;
    }
    Ok(())
}

/// Seal the durable fact that this attempt continued a lapsed lease. Listed
/// paths are retained in place; recording the fact never resets them.
pub(crate) fn seal_handover(
    attempt: &Directory,
    handover: &provenant_records::custody::Handover,
) -> Result<()> {
    crate::artifacts::seal(
        attempt,
        "handover.json",
        &crate::db::bytes(handover)?,
        crate::RECORD_LIMIT,
    )
}

/// Read the handover fact sealed for a lease successor, if any.
pub(crate) fn read_handover(
    attempt: &Directory,
) -> Result<Option<provenant_records::custody::Handover>> {
    let Some(raw) = crate::artifacts::read_sealed(attempt, "handover.json", crate::RECORD_LIMIT)?
    else {
        return Ok(None);
    };
    provenant_records::decode(
        raw.as_slice(),
        provenant_records::Limits {
            max_bytes: crate::RECORD_LIMIT,
            ..Default::default()
        },
    )
    .map(Some)
    .map_err(Into::into)
}

/// Launch writes the one worktree-slot lease confinement reads. Only the
/// holder and start time are replaced; the owning lane's sharing decision is
/// preserved so `lane share` survives a relaunch.
pub(crate) fn seal_worktree_lease(
    custody: Option<&(Directory, WriterBinding)>,
    tree: &provenant_confinement::VerifiedWorktree,
    scope: provenant_records::custody::Scope,
) -> Result<()> {
    let Some((slot, _)) = custody else {
        return Ok(());
    };
    let name = OsStr::new(worktrees::LEASE_FILE);
    let sharing = slot
        .read_stable(name, 4096)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            serde_json::from_value::<provenant_records::custody::Sharing>(value["sharing"].clone())
                .ok()
        })
        .unwrap_or_default();
    let since = worktrees::load_owner(tree)?
        .map_or_else(|| provenant_core::WallTime::MAX, |owner| owner.created_at);
    let lease = provenant_records::custody::WorktreeLease {
        holder: scope.attempt,
        since,
        sharing,
    };
    let bytes = serde_json::to_vec(&lease)
        .map_err(|_| RunError::Validation("worktree lease could not be encoded"))?;
    slot.atomic_write(
        name,
        ".lease-",
        &bytes,
        provenant_os::fs::PublishMode::Replace,
    )?
    .require_durable()?;
    Ok(())
}
