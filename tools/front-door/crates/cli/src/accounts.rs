//! Account registration at the CLI edge. No credentials enter config or output.
use clap::Subcommand;
use provenant_core::Roots;
use provenant_providers::accounts::{Registration, Settings};
use provenant_supervisor::{
    exec::SystemExecutor,
    fs::{Directory, LocalFs, Publication, PublishMode},
};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
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
}
pub fn execute(
    command: &Accounts,
    roots: &Roots,
    home: &Path,
    cwd: &Path,
) -> Result<Settings, String> {
    let mut settings =
        provenant_config::read_with_domains(roots, &LocalFs, &crate::domains::SECTIONS)
            .map_err(|e| e.to_string())?
            .domains
            .accounts;
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
        let directory =
            provenant_supervisor::fs::canonicalize(directory).map_err(|e| e.to_string())?;
        if adapter == "claude" && directory != home.join(format!(".claude-{label}")) {
            return Err(
                "Claude registration requires its fixed ~/.claude-<label> directory".into(),
            );
        }
        let executable = crate::run::provider_executable(adapter)?;
        let executor = SystemExecutor::new(1).map_err(|e| e.to_string())?;
        let user = provenant_supervisor::sys::account(provenant_supervisor::sys::Account::Current)
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
            cwd,
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
        };
        entry.registered().map_err(|e| e.to_string())?;
        // The config lock and held parent make the read/edit/replace one operation.
        let root = ensure_state_directory(roots.state.as_path())?;
        let _lock = root
            .try_lock(
                OsStr::new("config.lock"),
                provenant_supervisor::fs::LockMode::Exclusive,
            )
            .map_err(|e| e.to_string())?;
        settings = provenant_config::read_with_domains(roots, &LocalFs, &crate::domains::SECTIONS)
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
    match root
        .atomic_write(
            OsStr::new("config.toml"),
            ".accounts-",
            doc.to_string().as_bytes(),
            PublishMode::Replace,
        )
        .map_err(|e| e.to_string())?
    {
        Publication::Durable => {}
        Publication::VisibleDurabilityUnconfirmed { .. } => {
            return Err("account config durability unconfirmed".into());
        }
    }
    Ok(())
}

/// Records the organisation first seen for a legacy registration (trust on
/// first use). Best effort: the next validation retries on any failure.
pub fn record_org(roots: &Roots, adapter: &str, label: &str, org: &str) -> Result<(), String> {
    let root = ensure_state_directory(roots.state.as_path())?;
    let _lock = root
        .try_lock(
            OsStr::new("config.lock"),
            provenant_supervisor::fs::LockMode::Exclusive,
        )
        .map_err(|e| e.to_string())?;
    let mut settings =
        provenant_config::read_with_domains(roots, &LocalFs, &crate::domains::SECTIONS)
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
                if before.kind != provenant_supervisor::fs::FileKind::Directory
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
