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
    List,
    Register {
        label: String,
        #[arg(long)]
        adapter: String,
        #[arg(long)]
        directory: PathBuf,
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
        let key = provenant_providers::accounts::detect(
            &executor,
            adapter,
            &executable,
            home,
            &directory,
            cwd,
        )
        .map_err(|e| e.to_string())?;
        let entry = Registration {
            adapter: adapter.clone(),
            label: label.clone(),
            key,
            directory,
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
        settings
            .entries
            .retain(|_, a| a.adapter != *adapter || a.label != *label);
        settings.entries.insert(format!("{adapter}-{label}"), entry);
        if !provenant_config::DomainConfig::validate(&settings).is_empty() {
            return Err("conflicting account registration".into());
        }
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
            row["directory"] =
                toml_edit::value(e.directory.to_str().ok_or("account path encoding")?);
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
    }
    Ok(settings)
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
