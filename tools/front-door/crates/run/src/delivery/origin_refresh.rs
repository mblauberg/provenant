//! Coalesced refresh of shared origin tracking refs.

use super::{
    DeliveryPrepareStep, Directory, ORIGIN_REFRESH_LIMIT, OsStr, Result, RunError,
    delivery_advisory, prepare_error,
};
use serde::{Deserialize, Serialize};

pub(crate) fn coalesced_origin_refresh(
    common: &Directory,
    requested: &OriginRefreshRequest,
    cancel: &provenant_core::CancelToken,
    refresh: impl FnOnce() -> Result<()>,
) -> Result<()> {
    use provenant_core::{Clock, WaitBudget};
    use provenant_os::clock::SystemClock;
    let clock = SystemClock::new();
    coalesced_origin_refresh_with_budget(
        common,
        requested,
        cancel,
        WaitBudget::cli(clock.mono_now(), 60)
            .map_err(|error| prepare_error(DeliveryPrepareStep::OriginRefresh, error))?,
        refresh,
    )
}

pub(crate) fn coalesced_origin_refresh_with_budget(
    common: &Directory,
    requested: &OriginRefreshRequest,
    cancel: &provenant_core::CancelToken,
    lock_budget: provenant_core::WaitBudget,
    refresh: impl FnOnce() -> Result<()>,
) -> Result<()> {
    use provenant_os::{
        clock::SystemClock,
        fs::{FsError, LockMode},
    };
    let _fetch = match common.lock(
        OsStr::new("provenant-lane-origin-refresh.lock"),
        LockMode::Exclusive,
        lock_budget,
        cancel,
        &SystemClock::new(),
    ) {
        Ok(lock) => lock,
        Err(FsError::LockTimeout { .. }) => {
            delivery_advisory(
                "origin refresh is still busy; launching with existing tracking refs",
            );
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let latest = origin_refresh_completion(common)?;
    if latest
        .as_ref()
        .is_some_and(|latest| latest.boot == requested.boot && latest.micros >= requested.micros)
    {
        return Ok(());
    }
    match refresh() {
        Ok(()) => {
            let mut marker = OriginRefreshRequest::capture()?;
            if let Some(latest) = latest {
                marker.extra = latest.extra;
            }
            common
                .atomic_write(
                    OsStr::new("provenant-lane-origin-refresh"),
                    ".origin-refresh-",
                    &serde_json::to_vec(&marker).map_err(|error| {
                        prepare_error(DeliveryPrepareStep::OriginRefresh, error)
                    })?,
                    provenant_os::fs::PublishMode::Replace,
                )?
                .require_durable()?;
            Ok(())
        }
        // Another Git client can win a tracking-ref transaction without
        // participating in our fetch lock. Do not record failed refreshes.
        Err(RunError::DeliveryPrepare {
            step: DeliveryPrepareStep::Fetch,
            ref reason,
            ..
        }) if (reason.contains("cannot lock ref ")
            && reason.contains(" is at ")
            && reason.contains(" but expected "))
            || (reason.contains("Unable to create ")
                && reason.contains(".lock")
                && reason.contains("File exists"))
            || (reason.contains("fetching ref refs/remotes/")
                && reason.contains(" failed: reference already exists")) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct OriginRefreshRequest {
    #[serde(flatten)]
    pub(super) extra: provenant_records::ExtraFields,
    pub(super) boot: String,
    pub(super) micros: u64,
}
impl OriginRefreshRequest {
    pub(crate) fn capture() -> Result<Self> {
        Ok(Self {
            extra: Default::default(),
            boot: provenant_os::clock::boot_identity()
                .map_err(|error| prepare_error(DeliveryPrepareStep::OriginRefresh, error))?,
            micros: provenant_os::clock::host_monotonic_micros()
                .map_err(|error| prepare_error(DeliveryPrepareStep::OriginRefresh, error))?,
        })
    }
}
fn origin_refresh_completion(common: &Directory) -> Result<Option<OriginRefreshRequest>> {
    match common.read_bounded(
        OsStr::new("provenant-lane-origin-refresh"),
        ORIGIN_REFRESH_LIMIT,
    ) {
        Ok(bytes) => provenant_records::decode(
            bytes.as_slice(),
            provenant_records::Limits {
                max_bytes: ORIGIN_REFRESH_LIMIT,
                ..Default::default()
            },
        )
        .map(Some)
        .map_err(|error| prepare_error(DeliveryPrepareStep::OriginRefresh, error)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}
