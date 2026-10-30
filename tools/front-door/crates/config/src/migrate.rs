//! Rewrite retired routing timing keys to `schedule` in place. The edit goes
//! through the shared editor, so it takes the edit lock, validates the whole
//! result, records history and keeps comments and formatting. A timestamped
//! backup of the previous file is written first.
use crate::{ConfigError, EditOptions};
use provenant_core::Clock;
use provenant_os::fs::{Directory, PublishMode};
use serde::Serialize;
use toml_edit::{DocumentMut, Item, TableLike, Value};

/// Tables whose rows may carry the retired keys, under `domains.routing`, and
/// whether a row that has already expired is dropped instead of converted.
const TARGETS: [(&[&str], bool); 2] = [
    (&["pacing_overrides"], true),
    (&["model_policy", "rules"], false),
];

/// One rewritten or dropped record, located as `domains.routing.<path>.<index>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Migration {
    pub location: String,
    /// `moved` lists the keys rewritten; `dropped` an override that had expired.
    pub action: &'static str,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrateReport {
    pub dry_run: bool,
    pub migrations: Vec<Migration>,
    /// File name of the backup, relative to the state directory. Absent for a
    /// dry run or when nothing needed migrating.
    pub backup: Option<String>,
    pub before_revision: String,
    pub after_revision: String,
}

fn refused(location: &str, what: &str) -> ConfigError {
    ConfigError::Refused(format!("{location}: {what}"))
}

/// Migrate the user's `config.toml`. Nothing is written when no record carries
/// a retired key.
pub fn migrate<T>(
    roots: &provenant_core::Roots,
    sections: &crate::DomainSections<T>,
    dry_run: bool,
) -> Result<MigrateReport, ConfigError>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    let clock = provenant_os::clock::SystemClock::new();
    let now = clock
        .wall_now()
        .map_err(|_| ConfigError::Edit {
            reason: "clock unavailable",
        })?
        .as_timestamp();
    // Avoid creating the state directory or lock for a no-op. A real edit is
    // re-evaluated while holding the editor lock.
    if !dry_run {
        let preview = migrate(roots, sections, true)?;
        if preview.migrations.is_empty() {
            return Ok(MigrateReport {
                dry_run: false,
                after_revision: preview.before_revision.clone(),
                ..preview
            });
        }
    }
    let mut migrations = Vec::new();
    let mut backup = None;
    let options = EditOptions {
        dry_run,
        ..EditOptions::new("cli")
    };
    let edited = crate::edit::edit_document(roots, sections, &options, false, |document, _| {
        let original = document.to_string();
        migrations = rewrite(document, now.as_second())?;
        if !dry_run && !migrations.is_empty() {
            backup = Some(write_backup(roots, &original, now)?);
        }
        Ok(())
    })?;
    Ok(MigrateReport {
        dry_run,
        migrations,
        backup,
        before_revision: edited.before_revision,
        after_revision: edited.after_revision,
    })
}

fn write_backup(
    roots: &provenant_core::Roots,
    original: &str,
    now: jiff::Timestamp,
) -> Result<String, ConfigError> {
    let stamp = now.strftime("%Y%m%dT%H%M%SZ");
    let name = format!("config.toml.bak-{stamp}");
    let state = Directory::open_anchor(roots.state.as_path())?;
    state
        .atomic_write_with_permissions(
            std::ffi::OsStr::new(&name),
            ".config-bak-",
            original.as_bytes(),
            0o600,
            PublishMode::NoReplace,
        )?
        .require_durable()?;
    Ok(name)
}

/// Apply the rewrite to `document`; `now` is Unix seconds.
fn rewrite(document: &mut DocumentMut, now: i64) -> Result<Vec<Migration>, ConfigError> {
    let mut migrations = Vec::new();
    let Some(routing) = document
        .get_mut("domains")
        .and_then(Item::as_table_like_mut)
        .and_then(|domains| domains.get_mut("routing"))
        .and_then(Item::as_table_like_mut)
    else {
        return Ok(migrations);
    };
    for (path, drop_expired) in TARGETS {
        let Some(rows) = rows_at(&mut *routing, path) else {
            continue;
        };
        let prefix = format!("domains.routing.{}", path.join("."));
        let mut dropped = Vec::new();
        match rows {
            Item::ArrayOfTables(rows) => {
                for (index, row) in rows.iter_mut().enumerate() {
                    let location = format!("{prefix}.{index}");
                    match migrate_row(row, false, &location, drop_expired, now)? {
                        Outcome::Keep(Some(done)) => migrations.push(done),
                        Outcome::Keep(None) => {}
                        Outcome::Drop(done) => {
                            dropped.push(index);
                            migrations.push(done);
                        }
                    }
                }
                for index in dropped.into_iter().rev() {
                    rows.remove(index);
                }
            }
            Item::Value(Value::Array(rows)) => {
                for (index, row) in rows.iter_mut().enumerate() {
                    let Some(row) = row.as_inline_table_mut() else {
                        continue;
                    };
                    let location = format!("{prefix}.{index}");
                    match migrate_row(row, true, &location, drop_expired, now)? {
                        Outcome::Keep(Some(done)) => migrations.push(done),
                        Outcome::Keep(None) => {}
                        Outcome::Drop(done) => {
                            dropped.push(index);
                            migrations.push(done);
                        }
                    }
                }
                for index in dropped.into_iter().rev() {
                    rows.remove(index);
                }
            }
            _ => {}
        }
    }
    Ok(migrations)
}

fn rows_at<'a>(table: &'a mut dyn TableLike, path: &[&str]) -> Option<&'a mut Item> {
    let (first, rest) = path.split_first()?;
    let item = table.get_mut(first)?;
    if rest.is_empty() {
        Some(item)
    } else {
        rows_at(item.as_table_like_mut()?, rest)
    }
}

enum Outcome {
    Keep(Option<Migration>),
    Drop(Migration),
}

fn migrate_row(
    row: &mut dyn TableLike,
    inline: bool,
    location: &str,
    drop_expired: bool,
    now: i64,
) -> Result<Outcome, ConfigError> {
    let seconds = |row: &dyn TableLike, key: &str| -> Result<Option<String>, ConfigError> {
        let Some(item) = row.get(key) else {
            return Ok(None);
        };
        let at = item
            .as_integer()
            .and_then(|value| jiff::Timestamp::from_second(value).ok())
            .ok_or_else(|| refused(location, &format!("{key} is not a Unix timestamp")))?;
        Ok(Some(at.to_string()))
    };
    let starts = seconds(row, "starts_unix_seconds")?;
    let expires = seconds(row, "expires_unix_seconds")?;
    let windows = row.contains_key("windows");
    if starts.is_none() && expires.is_none() && !windows {
        return Ok(Outcome::Keep(None));
    }
    if drop_expired
        && let Some(at) = row.get("expires_unix_seconds").and_then(Item::as_integer)
        && at <= now
    {
        return Ok(Outcome::Drop(Migration {
            location: location.into(),
            action: "dropped",
            keys: vec!["expires_unix_seconds".into()],
        }));
    }
    let decorations = |key: &str| {
        row.get_key_value(key).and_then(|(key, item)| {
            item.as_value()
                .map(|value| (key.leaf_decor().clone(), value.decor().clone()))
        })
    };
    let starts_decor = decorations("starts_unix_seconds");
    let expires_decor = decorations("expires_unix_seconds");
    let schedule = row.entry("schedule").or_insert_with(|| {
        if inline {
            Item::Value(Value::InlineTable(toml_edit::InlineTable::new()))
        } else {
            Item::Table(toml_edit::Table::new())
        }
    });
    let Some(schedule) = schedule.as_table_like_mut() else {
        return Err(refused(location, "schedule is not a table"));
    };
    let mut keys = Vec::new();
    let mut place = |schedule: &mut dyn TableLike,
                     old: &str,
                     new: &str,
                     mut value: Value,
                     decor: Option<(toml_edit::Decor, toml_edit::Decor)>| {
        if schedule.contains_key(new) {
            return Err(refused(
                location,
                &format!("both {old} and schedule.{new} are set; remove one and rerun"),
            ));
        }
        if let Some((key_decor, value_decor)) = decor {
            *value.decor_mut() = value_decor;
            let mut key = toml_edit::Key::new(new);
            *key.leaf_decor_mut() = key_decor;
            schedule.entry_format(&key).or_insert(Item::Value(value));
        } else {
            schedule.insert(new, Item::Value(value));
        }
        keys.push(old.to_owned());
        Ok(())
    };
    if let Some(text) = starts {
        place(
            schedule,
            "starts_unix_seconds",
            "starts",
            Value::from(text),
            starts_decor,
        )?;
    }
    if let Some(text) = expires {
        place(
            schedule,
            "expires_unix_seconds",
            "expires",
            Value::from(text),
            expires_decor,
        )?;
    }
    // Remove the retired keys only after every placement succeeded.
    let moved = if windows { row.remove("windows") } else { None };
    for key in ["starts_unix_seconds", "expires_unix_seconds"] {
        row.remove(key);
    }
    if let Some(item) = moved {
        let Some(schedule) = row.get_mut("schedule").and_then(Item::as_table_like_mut) else {
            return Err(refused(location, "schedule is not a table"));
        };
        if schedule.contains_key("windows") {
            return Err(refused(
                location,
                "both windows and schedule.windows are set; remove one and rerun",
            ));
        }
        schedule.insert("windows", item);
        keys.push("windows".into());
    }
    Ok(Outcome::Keep(Some(Migration {
        location: location.into(),
        action: "moved",
        keys,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_comments_follow_migrated_keys() {
        let source = "[domains.routing]\n[[domains.routing.model_policy.rules]]\n# begin\nstarts_unix_seconds = 1700000000 # start\n# end\nexpires_unix_seconds = 1800000000 # expiry\n";
        let mut document: DocumentMut = source.parse().unwrap();
        rewrite(&mut document, 0).unwrap();
        let result = document.to_string();
        assert!(
            result.contains("# begin\nstarts = \"2023-11-14T22:13:20Z\" # start"),
            "{result}"
        );
        assert!(
            result.contains("# end\nexpires = \"2027-01-15T08:00:00Z\" # expiry"),
            "{result}"
        );
    }

    #[test]
    fn missing_config_migration_writes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let roots = crate::tests::state_roots(&state);
        let report = migrate(&roots, &crate::domains::NO_DOMAINS, false).unwrap();
        assert!(report.migrations.is_empty());
        assert_eq!(report.before_revision, report.after_revision);
        assert!(!state.exists());
        std::fs::create_dir(&state).unwrap();
        let original = "schema_version = 1\n# unchanged\n";
        std::fs::write(state.join("config.toml"), original).unwrap();
        let report = migrate(&roots, &crate::domains::NO_DOMAINS, false).unwrap();
        assert!(report.migrations.is_empty());
        assert_eq!(report.before_revision, report.after_revision);
        assert_eq!(
            std::fs::read_to_string(state.join("config.toml")).unwrap(),
            original
        );
        assert_eq!(std::fs::read_dir(&state).unwrap().count(), 1);
    }
}
