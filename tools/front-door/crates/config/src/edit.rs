//! The one config editor every command uses: take the shared edit lock,
//! compare the file revision, validate the complete edited configuration
//! through the registered domains, publish `<state>/config.toml` atomically
//! (mode 0600), record the change in the history, and read it back.
use crate::{
    ConfigError, ResolvedConfig,
    history::{FieldChange, HistoryEntry, HistoryLimits},
};
use provenant_core::{Clock, WaitBudget};
use provenant_os::fs::{Directory, PublishMode};
use serde::Serialize;
use std::{ffi::OsStr, io::Cursor};
use toml_edit::DocumentMut;

const MINIMAL: &str = "schema_version = 1\n";
const LOCK_WAIT_SECONDS: u64 = 5;

/// Who is editing, through what channel, and under which precondition.
#[derive(Debug, Clone)]
pub struct EditOptions<'a> {
    /// Refuse unless the file still has this revision.
    pub expect_revision: Option<&'a str>,
    /// Validate and report the change without writing anything.
    pub dry_run: bool,
    pub actor: String,
    pub channel: &'a str,
}
impl<'a> EditOptions<'a> {
    pub fn new(channel: &'a str) -> Self {
        Self {
            expect_revision: None,
            dry_run: false,
            actor: std::env::var("USER").unwrap_or_else(|_| "unknown".into()),
            channel,
        }
    }
}

/// The result of an edit: the configuration now in force (or, for a dry run,
/// the one that would be) and what changed.
pub struct Edited<T> {
    pub config: ResolvedConfig<T>,
    pub before_revision: String,
    pub after_revision: String,
    pub changes: Vec<FieldChange>,
    pub dry_run: bool,
}

/// The revision of the exact config bytes; an absent file is the empty text.
pub fn revision_of(text: Option<&str>) -> String {
    provenant_records::CanonicalV1::file_digest(text.unwrap_or_default().as_bytes()).to_string()
}

fn current_text(roots: &provenant_core::Roots) -> Result<Option<String>, ConfigError> {
    let path = roots.state.as_path().join("config.toml");
    crate::loader::open(roots)?
        .map(|reader| crate::loader::bounded_text(reader, &path))
        .transpose()
}

/// The configuration and the revision of the very bytes it was read from.
pub fn show<T: serde::de::DeserializeOwned + Serialize>(
    roots: &provenant_core::Roots,
    sections: &crate::DomainSections<T>,
) -> Result<(ResolvedConfig<T>, String), ConfigError> {
    let text = current_text(roots)?;
    let config = crate::loader::load_with_domains(
        roots,
        text.as_deref().map(|source| Cursor::new(source.as_bytes())),
        sections,
    )?;
    Ok((config, revision_of(text.as_deref())))
}

fn open_state(roots: &provenant_core::Roots) -> Result<Directory, ConfigError> {
    match Directory::open_anchor(roots.state.as_path()) {
        Ok(state) => Ok(state),
        Err(error) if error.is_not_found() => {
            // Create only the state directory itself, private. Its parent must
            // exist; a symlinked final component is refused by the anchor.
            let target = roots.state.as_path();
            let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
                return Err(error.into());
            };
            let parent = Directory::open_anchor(parent)?;
            match parent.mkdir(name, 0o700) {
                Ok(()) => parent.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            Ok(Directory::open_anchor(target)?)
        }
        Err(error) => Err(error.into()),
    }
}

/// Edit the owned TOML document. `edit` sees the document and the current
/// configuration. All changes commit or none do.
pub fn edit_config<T>(
    roots: &provenant_core::Roots,
    sections: &crate::DomainSections<T>,
    options: &EditOptions<'_>,
    edit: impl FnOnce(&mut DocumentMut, &ResolvedConfig<T>) -> Result<(), ConfigError>,
) -> Result<Edited<T>, ConfigError>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    edit_document(roots, sections, options, true, |document, current| {
        let current = current.ok_or(ConfigError::Edit {
            reason: "configuration unavailable",
        })?;
        edit(document, current)
    })
}

/// [`edit_config`], optionally without loading the current configuration
/// first. A repair of a configuration that does not load, such as a migration
/// of retired keys, cannot require the load it is repairing.
pub(crate) fn edit_document<T>(
    roots: &provenant_core::Roots,
    sections: &crate::DomainSections<T>,
    options: &EditOptions<'_>,
    load_first: bool,
    edit: impl FnOnce(&mut DocumentMut, Option<&ResolvedConfig<T>>) -> Result<(), ConfigError>,
) -> Result<Edited<T>, ConfigError>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    // A dry run writes nothing, so it neither creates the state directory
    // nor takes the lock.
    let held = if options.dry_run {
        None
    } else {
        let state = open_state(roots)?;
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), LOCK_WAIT_SECONDS)
            .map_err(|_| ConfigError::Locked)?;
        let guard = state
            .lock(
                OsStr::new("config.lock"),
                provenant_os::fs::LockMode::Exclusive,
                budget,
                &provenant_core::CancelToken::new(),
                &clock,
            )
            .map_err(|_| ConfigError::Locked)?;
        Some((state, guard))
    };

    let text = current_text(roots)?;
    let before_revision = revision_of(text.as_deref());
    if let Some(expected) = options.expect_revision
        && expected != before_revision
    {
        return Err(ConfigError::StaleRevision {
            current: before_revision,
        });
    }
    let current = load_first
        .then(|| {
            crate::loader::load_with_domains(
                roots,
                text.as_deref().map(|source| Cursor::new(source.as_bytes())),
                sections,
            )
        })
        .transpose()?;
    let mut document: DocumentMut =
        text.as_deref()
            .unwrap_or(MINIMAL)
            .parse()
            .map_err(|_| ConfigError::Edit {
                reason: "invalid TOML",
            })?;
    edit(&mut document, current.as_ref())?;
    let updated = document.to_string();
    // Reads recover invalid optional values; a write must refuse instead of
    // publishing a document whose effective value differs from the edit.
    crate::loader::validate_edit(&updated, sections)?;
    let config =
        crate::loader::load_with_domains(roots, Some(Cursor::new(updated.as_bytes())), sections)?;
    let changes = crate::history::changes(text.as_deref().unwrap_or(MINIMAL), &updated);
    let after_revision = revision_of(Some(&updated));

    let Some((state, _guard)) = held else {
        return Ok(Edited {
            config,
            before_revision,
            after_revision,
            changes,
            dry_run: true,
        });
    };
    if text.as_deref() != Some(updated.as_str()) {
        state
            .atomic_write_with_permissions(
                OsStr::new("config.toml"),
                ".config-",
                updated.as_bytes(),
                0o600,
                PublishMode::Replace,
            )?
            .require_durable()?;
        provenant_os::changes::hint(
            roots.state.as_path(),
            &[provenant_core::changes::Scope::Configuration],
        );
        if !changes.is_empty() {
            let now = provenant_os::clock::SystemClock::new()
                .wall_now()
                .map_err(|_| ConfigError::Edit {
                    reason: "clock unavailable",
                })?
                .as_timestamp();
            let entry = HistoryEntry {
                time: now.to_string(),
                actor: options.actor.clone(),
                channel: options.channel.into(),
                changes: changes.clone(),
                before_revision: before_revision.clone(),
                after_revision: after_revision.clone(),
            };
            // The edit is already committed; history is advisory and must not
            // turn a durable change into a reported failure.
            let _ = crate::history::append(
                &state,
                &entry,
                HistoryLimits::from_knobs(&config.knobs),
                now,
            );
        }
    }
    Ok(Edited {
        config: crate::loader::read_with_domains(roots, sections)?,
        before_revision,
        after_revision,
        changes,
        dry_run: false,
    })
}
