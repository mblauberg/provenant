//! Account directories of one harness stay identical for managed content.
//! Inspection only reads; `install` is the repair, because it already reaches
//! every registered account directory.
use crate::inspection::InspectionEvidence;
use crate::managed::{self, InstallError, InstallFinding, TargetId, Targets};
use crate::shared_settings;
use provenant_records::{AbsolutePath, Harness, InstallReceipt, OwnedValue};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::Path,
};

/// One kind of managed content a target of a harness owns.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Content {
    Skill(OsString),
    Selector(String),
}

/// `missing_account_directory`, `unusable_account_directory` and
/// `unsynced_account` for registrations that install has not reached, and `account_missing_entry` where a recorded
/// target lacks a skill, MCP entry or hook that a sibling target of the same
/// harness owns. Entries that exist but differ are `owned_entry_drift`.
pub(crate) fn findings(
    receipt: &InstallReceipt,
    stale_targets: &BTreeSet<TargetId>,
    targets: &Targets,
    evidence: &mut InspectionEvidence,
) -> Result<Vec<InstallFinding>, InstallError> {
    let state_root = targets.roots.state.as_path();
    let mut findings = Vec::new();
    let mut finding = |rule: &str, path: &Path| -> Result<(), InstallError> {
        findings.push(InstallFinding {
            rule: rule.into(),
            detail: None,
            path: AbsolutePath::new(path)?,
        });
        Ok(())
    };
    let live: Vec<_> = receipt
        .harness_targets
        .iter()
        .filter(|target| managed::recorded_id(target).is_ok_and(|id| !stale_targets.contains(&id)))
        .collect();
    for account in &targets.accounts {
        match managed::directory_scoped(&account.home, state_root, false) {
            Ok(_) => evidence.observe(b"account", &account.home, Some(b"present")),
            Err(InstallError::Fs(error)) if error.is_not_found() => {
                evidence.observe(b"account", &account.home, None);
                finding("missing_account_directory", &account.home)?;
                continue;
            }
            Err(InstallError::Fs(_)) => {
                evidence.observe(b"account", &account.home, Some(b"unusable"));
                finding("unusable_account_directory", &account.home)?;
                continue;
            }
            Err(error) => return Err(error),
        }
        let skills = account.target(&targets.home).skills_dir;
        let recorded = live.iter().any(|target| {
            target.harness == account.harness
                && target
                    .skills_dir
                    .to_path_buf()
                    .is_ok_and(|dir| dir == skills)
        });
        if !recorded {
            finding("unsynced_account", &account.home)?;
        }
    }
    let mut owned: BTreeMap<TargetId, BTreeSet<Content>> = BTreeMap::new();
    let mut union: BTreeMap<Harness, BTreeSet<Content>> = BTreeMap::new();
    for entry in &receipt.entries {
        let (Some(harness), Some(owner)) = (
            entry.harness,
            managed::entry_owner(entry, receipt, state_root)?,
        ) else {
            continue;
        };
        if stale_targets.contains(&owner) || entry.selector.starts_with(shared_settings::PREFIX) {
            continue;
        }
        let content = match &entry.expected {
            OwnedValue::Selector { .. } => Content::Selector(entry.selector.clone()),
            OwnedValue::Link { .. } if entry.selector == "skill" => {
                match entry.path.to_path_buf()?.file_name() {
                    Some(name) => Content::Skill(name.to_owned()),
                    None => continue,
                }
            }
            _ => continue,
        };
        union.entry(harness).or_default().insert(content.clone());
        owned.entry(owner).or_default().insert(content);
    }
    let mut settings_drift = Vec::new();
    if let Some(settings) = &targets.shared_settings {
        for target in &live {
            settings_findings(
                &targets.accounts,
                target,
                settings,
                receipt,
                state_root,
                evidence,
                &mut settings_drift,
            )?;
        }
    }
    for target in live {
        let id = managed::recorded_id(target)?;
        // An account that removed every hook on purpose is not out of step.
        let skips_hooks = target.hooks_relinquished;
        let have = owned.get(&id);
        for content in union.get(&target.harness).into_iter().flatten() {
            if have.is_some_and(|have| have.contains(content)) {
                continue;
            }
            if skips_hooks && matches!(content, Content::Selector(s) if s.starts_with("hook:")) {
                continue;
            }
            let path = match content {
                Content::Skill(name) => Some(target.skills_dir.to_path_buf()?.join(name)),
                Content::Selector(selector) if selector.starts_with("hook:") => target
                    .hook_file
                    .as_ref()
                    .map(AbsolutePath::to_path_buf)
                    .transpose()?,
                Content::Selector(_) => Some(target.mcp_file.to_path_buf()?),
            };
            if let Some(path) = path {
                finding("account_missing_entry", &path)?;
            }
        }
    }
    drop(finding);
    findings.extend(settings_drift);
    Ok(findings)
}

/// `settings_drift` where a Claude target lacks, or holds an older value of,
/// a shared key, and `settings_conflict` where its value was edited locally.
/// Each names the key. The source account's own file is never judged.
fn settings_findings(
    accounts: &[managed::AccountHome],
    target: &provenant_records::ResolvedHarnessTarget,
    settings: &shared_settings::SharedSettings,
    receipt: &InstallReceipt,
    state_root: &Path,
    evidence: &mut InspectionEvidence,
    findings: &mut Vec<InstallFinding>,
) -> Result<(), InstallError> {
    let Some(hook) = target
        .hook_file
        .as_ref()
        .filter(|_| target.harness == Harness::Claude)
    else {
        return Ok(());
    };
    let path = hook.to_path_buf()?;
    if shared_settings::canonical(&path) == shared_settings::canonical(&settings.source_file) {
        return Ok(());
    }
    let home = path.parent().map(shared_settings::canonical);
    let desired = settings.desired(home.as_deref());
    let bytes = match managed::snapshot_scoped_inspected(&path, state_root, Some(evidence))? {
        Some(managed::Snapshot {
            value: managed::Value::File { bytes, .. },
            ..
        }) => bytes,
        _ => b"{}".to_vec(),
    };
    for (key, want) in &desired {
        let current = crate::splice::get(&bytes, &[key.as_str()])?;
        if current.as_ref() == Some(want) {
            continue;
        }
        let last =
            receipt
                .entries
                .iter()
                .find_map(|entry| match (&entry.expected, entry.path == *hook) {
                    (OwnedValue::Selector { value, .. }, true)
                        if entry.selector.strip_prefix(shared_settings::PREFIX) == Some(key) =>
                    {
                        Some(value)
                    }
                    _ => None,
                });
        let rule = match (&current, last) {
            (None, Some(_)) => continue,
            (Some(have), Some(last)) if have != last => "settings_conflict",
            (Some(_), None) => "settings_conflict",
            _ => "settings_drift",
        };
        findings.push(InstallFinding {
            rule: rule.into(),
            detail: Some(shared_settings::detail(accounts, path.parent(), key)),
            path: hook.clone(),
        });
    }
    Ok(())
}
