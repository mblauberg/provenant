//! Account registration at the CLI edge. No credentials enter config or output.
use clap::{Subcommand, ValueEnum};
use provenant_core::Roots;
use provenant_os::{
    exec::SystemExecutor,
    fs::{Directory, PublishMode},
};
use provenant_providers::accounts::{Registration, Settings};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
/// Headline window choice; `default` clears a per-account override.
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum HeadlineArg {
    Auto,
    #[value(name = "5h")]
    FiveHour,
    Weekly,
    /// Follow the global choice (accounts set only)
    Default,
}
impl HeadlineArg {
    fn value(self) -> Option<provenant_quota::Headline> {
        match self {
            Self::Auto => Some(provenant_quota::Headline::Auto),
            Self::FiveHour => Some(provenant_quota::Headline::FiveHour),
            Self::Weekly => Some(provenant_quota::Headline::Weekly),
            Self::Default => None,
        }
    }
}
#[derive(Debug, Subcommand)]
pub enum Accounts {
    /// List registered accounts
    List,
    /// Register an account for an adapter
    Register {
        /// Account label
        label: String,
        /// Adapter the account belongs to, such as `claude`
        #[arg(long)]
        adapter: String,
        /// Account's configuration directory
        #[arg(long)]
        directory: PathBuf,
        /// Provider plan name
        #[arg(long)]
        plan: Option<String>,
        /// Account priority for quota scheduling
        #[arg(long)]
        priority: Option<u64>,
        /// Billing cycle anchor
        #[arg(long)]
        billing_anchor: Option<String>,
    },
    /// Repair managed settings, hooks and skills in every registered account
    ///
    /// Reapplies the installed release to each registered account directory and
    /// to every recorded install target, so all of them hold the same managed
    /// content. User-owned settings, edited hooks and credentials are never
    /// touched. `provenant check install` reports the drift this repairs.
    Sync {
        /// Plan the repair without changing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Set how an account appears in the menu bar
    ///
    /// ACCOUNT is an entry id such as `claude-work`, or a label that is unique
    /// across adapters. At least one option is required. Clear a field with an
    /// empty `--display-name`, `--order 0` or `--headline default`.
    Set {
        /// Entry id or unique label of the account
        account: String,
        /// Name shown in the menu bar (up to 64 characters); empty clears it
        #[arg(long)]
        display_name: Option<String>,
        /// Hide the account from the menu bar
        #[arg(long)]
        hidden: Option<bool>,
        /// Menu bar position, 1 to 999; 0 clears it
        #[arg(long)]
        order: Option<u16>,
        /// Window to lead with for this account: auto, 5h, weekly, or default
        /// to follow the global choice
        #[arg(long, value_enum)]
        headline: Option<HeadlineArg>,
    },
}

/// Resolves an entry id, or a label unique across adapters, to the entry id.
pub fn resolve(settings: &Settings, account: &str) -> Result<String, String> {
    if settings.entries.contains_key(account) {
        return Ok(account.to_owned());
    }
    let mut matches = settings
        .entries
        .iter()
        .filter(|(_, e)| e.label == account)
        .map(|(id, _)| id.clone());
    match (matches.next(), matches.next()) {
        (Some(id), None) => Ok(id),
        (Some(_), Some(_)) => Err(format!(
            "account `{account}` is ambiguous; use the entry id (see `provenant accounts list`)"
        )),
        _ => Err(format!("unknown account `{account}`")),
    }
}
/// Account configuration directories, in entry order. Only adapters whose
/// accounts are separate configuration directories (`registered_account`:
/// Claude and Codex) qualify. For the others the registered directory is an
/// identity or ledger location, not where the client reads skills and settings.
pub fn homes(settings: &Settings) -> Vec<provenant_config::AccountHome> {
    settings
        .entries
        .values()
        .filter_map(|entry| {
            let spec = provenant_records::AdapterSpec::get(&entry.adapter)?;
            spec.registered_account
                .then(|| provenant_config::AccountHome {
                    harness: spec.id,
                    label: entry.label.clone(),
                    home: entry.directory.clone(),
                })
        })
        .collect()
}

pub fn execute(command: &Accounts, roots: &Roots, home: &Path) -> Result<Settings, String> {
    let mut settings = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|e| e.to_string())?
        .domains
        .accounts;
    if let Accounts::Set {
        account,
        display_name,
        hidden,
        order,
        headline,
    } = command
    {
        if display_name.is_none() && hidden.is_none() && order.is_none() && headline.is_none() {
            return Err("accounts set requires at least one option".into());
        }
        let root = ensure_state_directory(roots.state.as_path())?;
        let _lock = root
            .try_lock(
                OsStr::new("config.lock"),
                provenant_os::fs::LockMode::Exclusive,
            )
            .map_err(|e| e.to_string())?;
        settings = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
            .map_err(|e| e.to_string())?
            .domains
            .accounts;
        let id = resolve(&settings, account)?;
        let entry = settings.entries.get_mut(&id).ok_or("unknown account")?;
        if let Some(name) = display_name {
            let name = name.trim();
            entry.display_name = (!name.is_empty()).then(|| name.to_owned());
        }
        if let Some(hidden) = hidden {
            entry.hidden = *hidden;
        }
        if let Some(order) = order {
            entry.order = (*order != 0).then_some(*order);
        }
        if let Some(headline) = headline {
            entry.headline = headline.value();
        }
        if !provenant_config::DomainConfig::validate(&settings).is_empty() {
            return Err("invalid account display settings".into());
        }
        write_settings(&root, &settings)?;
        return Ok(settings);
    }
    if let Accounts::Register {
        label,
        adapter,
        directory,
        plan,
        priority,
        billing_anchor,
    } = command
    {
        if !provenant_providers::accounts::label(label) {
            return Err("invalid account label".into());
        }
        let directory = provenant_os::fs::canonicalize(directory).map_err(|e| e.to_string())?;
        if adapter == "claude" && directory != home.join(format!(".claude-{label}")) {
            return Err(
                "Claude registration requires its fixed ~/.claude-<label> directory".into(),
            );
        }
        let executable = crate::run::provider_executable(adapter)?;
        // The probe root must exist first. The config lock is taken later and
        // is not held over the probe, which can run for seconds.
        ensure_state_directory(roots.state.as_path())?;
        // The probe runs in a fresh private directory under the host state
        // root, never the caller's cwd, which may hold lane-written settings.
        let system = SystemExecutor::new(1).map_err(|e| e.to_string())?;
        let executor =
            provenant_providers::host::HostProbeExecutor::new(&system, roots.state.as_path())
                .map_err(|e| e.to_string())?;
        let user = provenant_os::sys::account(provenant_os::sys::Account::Current)
            .map_err(|e| e.to_string())?
            .ok_or("current user identity is unavailable")?
            .name;
        let detected = provenant_providers::accounts::detect_with_org(
            &executor,
            adapter,
            &executable,
            home,
            &user,
            &directory,
            roots.state.as_path(),
        );
        // OpenCode's local ledger has no sign-in identity.
        let (key, org_fingerprint) = match detected {
            Err(provenant_providers::ProviderError::UnknownIdentity) if adapter == "opencode" => {
                (format!("{adapter}-{label}"), None)
            }
            other => other.map_err(|e| e.to_string())?,
        };
        let mut entry = Registration {
            adapter: adapter.clone(),
            label: label.clone(),
            key,
            org_fingerprint,
            directory,
            plan: plan.clone(),
            priority: *priority,
            billing_anchor: billing_anchor.clone(),
            limits: None,
            display_name: None,
            hidden: false,
            order: None,
            headline: None,
        };
        entry.registered().map_err(|e| e.to_string())?;
        // The config lock and held parent make the read/edit/replace one operation.
        let root = ensure_state_directory(roots.state.as_path())?;
        let _lock = root
            .try_lock(
                OsStr::new("config.lock"),
                provenant_os::fs::LockMode::Exclusive,
            )
            .map_err(|e| e.to_string())?;
        settings = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
            .map_err(|e| e.to_string())?
            .domains
            .accounts;
        if let Some(previous) = settings
            .entries
            .values()
            .find(|a| a.adapter == *adapter && a.label == *label)
        {
            entry.plan = entry.plan.or_else(|| previous.plan.clone());
            entry.priority = entry.priority.or(previous.priority);
            entry.billing_anchor = entry
                .billing_anchor
                .or_else(|| previous.billing_anchor.clone());
            entry.limits = previous.limits.clone();
            entry.display_name = previous.display_name.clone();
            entry.hidden = previous.hidden;
            entry.order = previous.order;
            entry.headline = previous.headline;
        }
        settings
            .entries
            .retain(|_, a| a.adapter != *adapter || a.label != *label);
        settings.entries.insert(format!("{adapter}-{label}"), entry);
        if !provenant_config::DomainConfig::validate(&settings).is_empty() {
            return Err("conflicting account registration".into());
        }
        write_settings(&root, &settings)?;
    }
    Ok(settings)
}

fn headline_name(value: provenant_quota::Headline) -> &'static str {
    match value {
        provenant_quota::Headline::Auto => "auto",
        provenant_quota::Headline::FiveHour => "5h",
        provenant_quota::Headline::Weekly => "weekly",
    }
}

/// Writes `[domains.quota.display]` under `config.lock`, leaving the rest of the file intact.
/// `icon` is an account reference resolved to its entry id, or `most_constrained`.
pub fn set_quota_display(
    roots: &Roots,
    headline: Option<provenant_quota::Headline>,
    icon: Option<&str>,
) -> Result<(), String> {
    if headline.is_none() && icon.is_none() {
        return Ok(());
    }
    let root = ensure_state_directory(roots.state.as_path())?;
    let _lock = root
        .try_lock(
            OsStr::new("config.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )
        .map_err(|e| e.to_string())?;
    let domains = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|e| e.to_string())?
        .domains;
    let icon = match icon {
        Some(provenant_quota::MOST_CONSTRAINED) => Some(provenant_quota::MOST_CONSTRAINED.into()),
        Some(account) => Some(resolve(&domains.accounts, account)?),
        None => None,
    };
    let bytes = match root.read_bounded(OsStr::new("config.toml"), 1024 * 1024) {
        Ok(b) => b,
        Err(e) if e.is_not_found() => b"schema_version = 1\n".to_vec(),
        Err(e) => return Err(e.to_string()),
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| "invalid config encoding")?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|_| "invalid config")?;
    if !doc.contains_key("domains") {
        doc["domains"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let table = |item: &mut toml_edit::Item| -> Result<(), String> {
        if item.is_none() {
            *item = toml_edit::Item::Table(toml_edit::Table::new());
        }
        item.as_table_mut()
            .map(|_| ())
            .ok_or_else(|| "config domains must be TOML tables".to_owned())
    };
    table(&mut doc["domains"])?;
    table(&mut doc["domains"]["quota"])?;
    table(&mut doc["domains"]["quota"]["display"])?;
    let display = &mut doc["domains"]["quota"]["display"];
    if let Some(headline) = headline {
        display["headline"] = toml_edit::value(headline_name(headline));
    }
    if let Some(icon) = icon {
        display["icon"] = toml_edit::value(icon);
    }
    root.atomic_write(
        OsStr::new("config.toml"),
        ".quota-display-",
        doc.to_string().as_bytes(),
        PublishMode::Replace,
    )
    .map_err(|e| e.to_string())?
    .require_durable()
    .map_err(|_| "quota display config durability unconfirmed".into())
}

fn write_settings(root: &Directory, settings: &Settings) -> Result<(), String> {
    let bytes = match root.read_bounded(OsStr::new("config.toml"), 1024 * 1024) {
        Ok(b) => b,
        Err(e) if e.is_not_found() => b"schema_version = 1\n".to_vec(),
        Err(e) => return Err(e.to_string()),
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| "invalid config encoding")?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|_| "invalid config")?;
    let mut section = toml_edit::Table::new();
    let mut entries = toml_edit::Table::new();
    for (name, e) in &settings.entries {
        let mut row = toml_edit::Table::new();
        row["adapter"] = toml_edit::value(&e.adapter);
        row["label"] = toml_edit::value(&e.label);
        row["key"] = toml_edit::value(&e.key);
        if let Some(value) = &e.org_fingerprint {
            row["org_fingerprint"] = toml_edit::value(value);
        }
        row["directory"] = toml_edit::value(e.directory.to_str().ok_or("account path encoding")?);
        if let Some(value) = &e.plan {
            row["plan"] = toml_edit::value(value);
        }
        if let Some(value) = e.priority {
            let value = i64::try_from(value).map_err(|_| "priority out of range")?;
            row["priority"] = toml_edit::value(value);
        }
        if let Some(value) = &e.billing_anchor {
            row["billing_anchor"] = toml_edit::value(value);
        }
        if let Some(value) = &e.display_name {
            row["display_name"] = toml_edit::value(value);
        }
        if e.hidden {
            row["hidden"] = toml_edit::value(true);
        }
        if let Some(value) = e.order {
            row["order"] = toml_edit::value(i64::from(value));
        }
        if let Some(value) = e.headline {
            row["headline"] = toml_edit::value(headline_name(value));
        }
        if let Some(limits) = &e.limits {
            let mut table = toml_edit::Table::new();
            if let Some(value) = limits.monthly_usd {
                table["monthly_usd"] = toml_edit::value(value);
            }
            if let Some(value) = &limits.cycle_anchor {
                table["cycle_anchor"] = toml_edit::value(value);
            }
            if let Some(value) = &limits.weekly_anchor {
                table["weekly_anchor"] = toml_edit::value(value);
            }
            row["limits"] = toml_edit::Item::Table(table);
        }
        entries[name] = toml_edit::Item::Table(row);
    }
    section["entries"] = toml_edit::Item::Table(entries);
    if !doc.contains_key("domains") {
        doc["domains"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let domains = doc
        .get_mut("domains")
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or("domains must be a TOML table")?;
    domains["accounts"] = toml_edit::Item::Table(section);
    root.atomic_write(
        OsStr::new("config.toml"),
        ".accounts-",
        doc.to_string().as_bytes(),
        PublishMode::Replace,
    )
    .map_err(|e| e.to_string())?
    .require_durable()
    .map_err(|_| "account config durability unconfirmed")?;
    Ok(())
}

/// Records the organisation first seen for a legacy registration (trust on
/// first use). Best effort: the next validation retries on any failure.
pub fn record_org(roots: &Roots, adapter: &str, label: &str, org: &str) -> Result<(), String> {
    let root = ensure_state_directory(roots.state.as_path())?;
    let _lock = root
        .try_lock(
            OsStr::new("config.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )
        .map_err(|e| e.to_string())?;
    let mut settings = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|e| e.to_string())?
        .domains
        .accounts;
    let entry = settings
        .entries
        .values_mut()
        .find(|a| a.adapter == adapter && a.label == label)
        .ok_or("account is not registered")?;
    if entry.org_fingerprint.is_some() {
        return Ok(());
    }
    entry.org_fingerprint = Some(org.to_owned());
    write_settings(&root, &settings)
}

fn ensure_state_directory(path: &Path) -> Result<Directory, String> {
    if !path.is_absolute() {
        return Err("state directory must be absolute".into());
    }
    let mut dir = Directory::root(true).map_err(|e| e.to_string())?;
    for part in path.components() {
        let name = match part {
            std::path::Component::Normal(name) => name,
            std::path::Component::RootDir => continue,
            _ => return Err("state directory must be normalised".into()),
        };
        dir = match dir.open_verified_directory(name) {
            Ok(next) => next,
            Err(e) if e.is_not_found() => {
                dir.mkdir(name, 0o700).map_err(|e| e.to_string())?;
                dir.sync().map_err(|e| e.to_string())?;
                dir.open_verified_directory(name)
                    .map_err(|e| e.to_string())?
            }
            // The configured absolute state hierarchy may cross a mount.
            // Match worktree creation: verify a held no-follow child before
            // establishing its new device boundary; descendants stay strict.
            Err(e) if e.is_cross_device() => {
                let before = dir.metadata_at(name, false).map_err(|e| e.to_string())?;
                let child = dir.open_directory(name).map_err(|e| e.to_string())?;
                let opened = child.metadata().map_err(|e| e.to_string())?;
                let after = dir.metadata_at(name, false).map_err(|e| e.to_string())?;
                if before.kind != provenant_os::fs::FileKind::Directory
                    || !before.same_identity(&opened)
                    || !opened.same_identity(&after)
                {
                    return Err("state directory changed while opening".into());
                }
                child.into_anchor().map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        };
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registration(
        adapter: &str,
        label: &str,
        directory: &str,
    ) -> provenant_providers::accounts::Registration {
        provenant_providers::accounts::Registration {
            adapter: adapter.into(),
            label: label.into(),
            key: format!("{adapter}-{label}"),
            org_fingerprint: None,
            directory: directory.into(),
            plan: None,
            priority: None,
            billing_anchor: None,
            limits: None,
            display_name: None,
            hidden: false,
            order: None,
            headline: None,
        }
    }

    #[test]
    fn only_accounts_that_are_configuration_directories_become_install_targets() {
        let mut settings = Settings::default();
        for (adapter, label, directory) in [
            ("claude", "work", "/home/u/.claude-work"),
            ("codex", "alt", "/home/u/.codex-alt"),
            ("cursor", "me", "/home/u/Library/Cursor"),
            ("opencode", "local", "/home/u/ledger"),
        ] {
            settings.entries.insert(
                format!("{adapter}-{label}"),
                registration(adapter, label, directory),
            );
        }
        let homes: Vec<_> = homes(&settings)
            .into_iter()
            .map(|account| (account.harness.spec().name, account.home))
            .collect();
        assert_eq!(
            homes,
            [
                ("claude", PathBuf::from("/home/u/.claude-work")),
                ("codex", PathBuf::from("/home/u/.codex-alt")),
            ]
        );
    }
}
