//! Independent reservation receipts prevent busy quota publications losing launches.
use super::*;
const DIRECTORY: &str = ".allowance-pending";
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DeferredLaunch {
    account: AccountScope,
    launch: AllowanceLaunch,
}
pub(super) fn record(
    dir: &Directory,
    scope: &AccountScope,
    id: &str,
    at: i64,
    cost: u64,
) -> Result<(), EdgeError> {
    let mut ledger = AllowanceLedger::default();
    ledger.record_launch(id, at, cost)?;
    let pending = child(dir, OsStr::new(DIRECTORY))?;
    let account = child(&pending, OsStr::new(&scope.account_key))?;
    let bytes = serde_json::to_vec(&DeferredLaunch {
        account: scope.clone(),
        launch: ledger.recent_launches.remove(0),
    })?;
    durable(account.atomic_write_with_permissions(
        OsStr::new(&format!("{}-{id}.json", scope.adapter)),
        ".launch",
        &bytes,
        0o600,
        PublishMode::Replace,
    )?)
}
/// Read-only overlay. A publisher cannot erase receipts it did not incorporate.
pub(super) fn overlay(dir: &Directory, records: &mut Vec<AccountRecord>) -> Result<(), EdgeError> {
    // Persisted acknowledgements prevent replay after a crash between account
    // publication and receipt retirement, even beyond the rolling launch journal.
    let acknowledged: std::collections::BTreeMap<_, _> = records
        .iter_mut()
        .map(|r| {
            (
                (r.account.adapter.clone(), r.account.account_key.clone()),
                std::mem::take(&mut r.allowance.receipt_launch_ids),
            )
        })
        .collect();
    let pending = match dir.open_verified_directory(OsStr::new(DIRECTORY)) {
        Ok(dir) => dir,
        Err(e) if e.is_not_found() => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    check_private(&pending)?;
    let mut launches = Vec::new();
    for account in pending.entries()? {
        let account = account?;
        let directory = pending.open_verified_directory(&account)?;
        check_private(&directory)?;
        for file in directory.entries()? {
            let file = file?;
            if file.to_str().is_some_and(|s| s.starts_with('.')) {
                continue;
            }
            let mut bytes = Vec::new();
            let reader = match directory.open_read(&file) {
                Ok(r) => r,
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(e.into()),
            };
            let meta = reader.metadata()?;
            if meta.mode & 0o777 != 0o600 || meta.uid != provenant_os::fs::effective_uid() {
                return Err(EdgeError::Permissions);
            }
            reader
                .take(MAX_RECORD_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|source| EdgeError::Read {
                    path: PathBuf::from(&file),
                    source,
                })?;
            let item: DeferredLaunch = serde_json::from_slice(&bytes)?;
            item.account.validate()?;
            let mut ledger = AllowanceLedger::default();
            ledger.record_launch(
                &item.launch.id,
                item.launch.unix_seconds,
                item.launch.cost_basis_points,
            )?;
            if account != item.account.account_key.as_str()
                || file != format!("{}-{}.json", item.account.adapter, item.launch.id).as_str()
                || !(1..=10_000).contains(&item.launch.cost_basis_points)
            {
                return Err(QuotaError::Invalid("deferred allowance identity").into());
            }
            launches.push(item);
        }
    }
    launches.sort_by_key(|l| l.launch.unix_seconds);
    for item in launches {
        let pos = records.iter().position(|r| {
            r.account.adapter == item.account.adapter
                && r.account.account_key == item.account.account_key
        });
        let record = match pos {
            Some(p) => &mut records[p],
            None => {
                if records.len() == MAX_ACCOUNTS {
                    return Err(QuotaError::Invalid("account bound").into());
                }
                let index = records.len();
                records.push(AccountRecord::empty(item.account));
                &mut records[index]
            }
        };
        let key = (
            record.account.adapter.clone(),
            record.account.account_key.clone(),
        );
        if !acknowledged
            .get(&key)
            .is_some_and(|ids| ids.contains(&item.launch.id))
            && !record
                .allowance
                .receipt_launch_ids
                .contains(&item.launch.id)
        {
            record.allowance.record_launch(
                &item.launch.id,
                item.launch.unix_seconds,
                item.launch.cost_basis_points,
            )?;
        }
        record.allowance.receipt_launch_ids.insert(item.launch.id);
    }
    Ok(())
}
pub(super) fn retire(dir: &Directory, record: &AccountRecord) -> Result<(), EdgeError> {
    let pending = match dir.open_verified_directory(OsStr::new(DIRECTORY)) {
        Ok(dir) => dir,
        Err(e) if e.is_not_found() => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let account = match pending.open_verified_directory(OsStr::new(&record.account.account_key)) {
        Ok(dir) => dir,
        Err(e) if e.is_not_found() => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for id in &record.allowance.receipt_launch_ids {
        match account.unlink(OsStr::new(&format!("{}-{id}.json", record.account.adapter))) {
            Ok(()) => (),
            Err(e) if e.is_not_found() => (),
            Err(e) => return Err(e.into()),
        }
    }
    account.sync()?;
    Ok(())
}
