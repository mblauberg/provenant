//! Account registration at the CLI edge. No credentials enter config or output.
use clap::{Subcommand, ValueEnum};
use provenant_core::Roots;
use provenant_os::{exec::SystemExecutor, fs::Directory};
use provenant_providers::accounts::{Registration, Settings};
use std::path::{Path, PathBuf};
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
    /// Bring every registered account directory to the installed release
    ///
    /// Reapplies the installed release to each registered account directory and
    /// to every recorded install target. What is synced: the managed hooks, the
    /// managed skills, the Fabric MCP entry, and the allow-listed keys of the
    /// shared Claude `settings.json` (`[domains.accounts.shared_settings]`).
    /// Never touched: identity and credentials, `.claude.json` identity fields,
    /// settings keys outside the allow-list, and hooks you edited or removed.
    /// A shared setting you edited in one account is reported as
    /// `settings_conflict`, not overwritten. `provenant check install` reports
    /// the drift this repairs.
    Sync {
        /// Plan the repair without changing anything
        #[arg(long)]
        dry_run: bool,
        /// Pull this account's locally edited shared settings into the source
        /// settings file, then sync every account (ACCOUNT as for `accounts set`)
        #[arg(long, value_name = "ACCOUNT")]
        adopt: Option<String>,
        /// Overwrite locally edited shared settings with the source's values
        #[arg(long)]
        force_settings: bool,
        /// Remove managed content from install targets whose account is no
        /// longer registered
        #[arg(long)]
        prune_orphans: bool,
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
/// Directories are canonical (symlinks and `..` resolved), and two
/// registrations of one directory count once.
pub fn homes(settings: &Settings) -> Vec<provenant_config::AccountHome> {
    let mut seen = std::collections::BTreeSet::new();
    settings
        .entries
        .values()
        .filter_map(|entry| {
            let spec = provenant_records::AdapterSpec::get(&entry.adapter)?;
            if !spec.registered_account {
                return None;
            }
            let home = provenant_config::shared_settings::canonical(&entry.directory);
            seen.insert((spec.name, home.clone()))
                .then(|| provenant_config::AccountHome {
                    harness: spec.id,
                    label: entry.label.clone(),
                    home,
                })
        })
        .collect()
}

pub fn execute(command: &Accounts, roots: &Roots, home: &Path) -> Result<Settings, String> {
    let settings = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
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
        return edit_accounts(roots, |mut settings| {
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
            Ok(settings)
        });
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
            plans: Vec::new(),
            directories: Vec::new(),
            limits: None,
            display_name: None,
            hidden: false,
            order: None,
            headline: None,
            settings: Default::default(),
        };
        entry.registered().map_err(|e| e.to_string())?;
        // The config lock and revision check make read/edit/replace one operation.
        return edit_accounts(roots, |mut settings| {
            if let Some(previous) = settings
                .entries
                .values()
                .find(|a| a.adapter == *adapter && a.label == *label)
            {
                entry.plan = entry.plan.take().or_else(|| previous.plan.clone());
                entry.priority = entry.priority.or(previous.priority);
                entry.billing_anchor = entry
                    .billing_anchor
                    .take()
                    .or_else(|| previous.billing_anchor.clone());
                entry.limits = previous.limits.clone();
                entry.plans = previous.plans.clone();
                entry.directories = previous.directories.clone();
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
            Ok(settings)
        });
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
    ensure_state_directory(roots.state.as_path())?;
    let mut refusal = None;
    let edited = provenant_config::edit_config(
        roots,
        &crate::domains::SECTIONS,
        &provenant_config::EditOptions::new("cli"),
        |doc, current| {
            let icon = match icon {
                Some(provenant_quota::MOST_CONSTRAINED) => {
                    Ok(Some(provenant_quota::MOST_CONSTRAINED.into()))
                }
                Some(account) => resolve(&current.domains.accounts, account).map(Some),
                None => Ok(None),
            }
            .map_err(|reason| {
                refusal = Some(reason.clone());
                provenant_config::ConfigError::Refused(reason)
            })?;
            let display = |reason: &'static str| provenant_config::ConfigError::Edit { reason };
            let domains = table_at(doc.as_item_mut(), "domains")
                .ok_or(display("config domains must be TOML tables"))?;
            let quota =
                table_at(domains, "quota").ok_or(display("config domains must be TOML tables"))?;
            let section =
                table_at(quota, "display").ok_or(display("config domains must be TOML tables"))?;
            if let Some(headline) = headline {
                section["headline"] = toml_edit::value(headline_name(headline));
            }
            if let Some(icon) = icon {
                section["icon"] = toml_edit::value(icon);
            }
            Ok(())
        },
    );
    edited
        .map(|_| ())
        .map_err(|error| refusal.unwrap_or_else(|| error.to_string()))
}

/// The child table `key`, created when absent.
fn table_at<'a>(parent: &'a mut toml_edit::Item, key: &str) -> Option<&'a mut toml_edit::Item> {
    let item = &mut parent[key];
    if item.is_none() {
        *item = toml_edit::Item::Table(toml_edit::Table::new());
    }
    item.is_table().then_some(item)
}

fn apply_settings(doc: &mut toml_edit::DocumentMut, settings: &Settings) -> Result<(), String> {
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
        if !e.directories.is_empty() {
            let mut list = toml_edit::Array::new();
            for d in &e.directories {
                list.push(d.to_str().ok_or("account path encoding")?);
            }
            row["directories"] = toml_edit::value(list);
        }
        if !e.plans.is_empty() {
            let mut plans = toml_edit::ArrayOfTables::new();
            for plan in &e.plans {
                let mut table = toml_edit::Table::new();
                table["name"] = toml_edit::value(&plan.name);
                table["price"] = toml_edit::value(plan.price);
                table["currency"] = toml_edit::value(&plan.currency);
                if let Some(value) = plan.notional_usd {
                    table["notional_usd"] = toml_edit::value(value);
                }
                table["effective_from"] = toml_edit::value(&plan.effective_from);
                plans.push(table);
            }
            row["plans"] = toml_edit::Item::ArrayOfTables(plans);
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
    Ok(())
}

/// Records the organisation first seen for a legacy registration (trust on
/// first use). Best effort: the next validation retries on any failure.
pub fn record_org(roots: &Roots, adapter: &str, label: &str, org: &str) -> Result<(), String> {
    edit_accounts(roots, |mut settings| {
        let entry = settings
            .entries
            .values_mut()
            .find(|a| a.adapter == adapter && a.label == label)
            .ok_or("account is not registered")?;
        if entry.org_fingerprint.is_none() {
            entry.org_fingerprint = Some(org.to_owned());
        }
        Ok(settings)
    })
    .map(|_| ())
}

/// Edit the accounts section under the shared config lock and revision check.
/// `change` sees the current settings and returns the settings to write.
fn edit_accounts(
    roots: &Roots,
    change: impl FnOnce(Settings) -> Result<Settings, String>,
) -> Result<Settings, String> {
    ensure_state_directory(roots.state.as_path())?;
    let mut refusal = None;
    let edited = provenant_config::edit_config(
        roots,
        &crate::domains::SECTIONS,
        &provenant_config::EditOptions::new("cli"),
        |doc, current| match change(current.domains.accounts.clone())
            .and_then(|settings| apply_settings(doc, &settings))
        {
            Ok(()) => Ok(()),
            Err(reason) => {
                refusal = Some(reason.clone());
                Err(provenant_config::ConfigError::Refused(reason))
            }
        },
    );
    match edited {
        Ok(edited) => Ok(edited.config.domains.accounts),
        Err(error) => Err(refusal.unwrap_or_else(|| error.to_string())),
    }
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
            plans: Vec::new(),
            directories: Vec::new(),
            limits: None,
            display_name: None,
            hidden: false,
            order: None,
            headline: None,
            settings: Default::default(),
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

    #[test]
    fn registered_directories_are_canonical_and_counted_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let real = temp.path().canonicalize()?.join(".claude-work");
        std::fs::create_dir(&real)?;
        let link = temp.path().join("alias");
        std::os::unix::fs::symlink(&real, &link)?;
        let mut settings = Settings::default();
        for (id, label, directory) in [
            ("a", "work", real.clone()),
            ("b", "again", link),
            ("c", "dotted", real.join("..").join(".claude-work")),
        ] {
            settings.entries.insert(
                id.into(),
                registration("claude", label, &directory.to_string_lossy()),
            );
        }
        let homes = homes(&settings);
        assert_eq!(homes.len(), 1, "{homes:?}");
        assert_eq!(homes[0].home, real);
        Ok(())
    }
}
