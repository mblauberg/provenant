use rusqlite::Connection;
use std::{cell::RefCell, collections::HashMap};

/// A store's immutable base within a root format major and its ordered additive
/// declarations.
#[derive(Clone, Copy, Debug)]
pub struct Schema {
    /// Base DDL, ending in `PRAGMA user_version=<version>;`.
    pub sql: &'static str,
    /// Per-store base identifier fixed within a state-root format major. It
    /// advances only with the root major, never as an independent minor count.
    pub version: i64,
    pub steps: &'static [Step],
    /// One-shot rewrites from an older base version. A writable open applies
    /// them in order and records the new base; a reader refuses until then.
    pub migrations: &'static [Migration],
}

/// A one-shot rewrite of stored rows between two base versions. Unlike an
/// additive step it changes data, runs once, and raises `user_version` to `to`.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    pub from: i64,
    pub to: i64,
    /// SQL executed inside the writable-open transaction. It must not set
    /// `user_version`; the opener records `to` after it succeeds.
    pub sql: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct Step {
    /// Release minor that first declared this object; not persisted in SQLite.
    pub minor: u32,
    pub kind: StepKind,
}

#[derive(Clone, Copy, Debug)]
pub enum StepKind {
    Table {
        name: &'static str,
        sql: &'static str,
    },
    Index {
        name: &'static str,
        sql: &'static str,
    },
    /// Nullable, or NOT NULL with a non-NULL constant DEFAULT.
    Column {
        table: &'static str,
        column: &'static str,
        decl: &'static str,
    },
}

pub const RESET_CODE: &str = "STATE_RESET_REQUIRED";

#[derive(Debug)]
pub enum VersionError {
    Refused { found: i64, expected: i64 },
    MigrationRequired { found: i64, expected: i64 },
    MissingBaseTable { table: String },
    MissingStep { minor: u32, object: String },
    InvalidStep { minor: u32, reason: &'static str },
    InvalidMigration { from: i64, reason: &'static str },
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for VersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { found, expected } => write!(
                f,
                "{RESET_CODE}: store base version {found} is not {expected}; use a fresh state root"
            ),
            Self::MigrationRequired { found, expected } => write!(
                f,
                "store base version {found} needs a writable open to migrate to {expected}"
            ),
            Self::MissingBaseTable { table } => {
                write!(f, "{RESET_CODE}: store is missing known base table {table}")
            }
            Self::MissingStep { minor, object } => write!(
                f,
                "store is missing schema object {object} from minor {minor}; a writable open applies it"
            ),
            Self::InvalidStep { minor, reason } => write!(
                f,
                "invalid additive schema step from minor {minor}: {reason}"
            ),
            Self::InvalidMigration { from, reason } => write!(
                f,
                "invalid data migration from base version {from}: {reason}"
            ),
            Self::Sqlite(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for VersionError {}
impl From<rusqlite::Error> for VersionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

// Each worker parses a base once. No shared lock on the store-open path.
thread_local! {
    static BASE_TABLES: RefCell<HashMap<&'static str, Vec<String>>> = RefCell::new(HashMap::new());
}

fn base_tables(schema: Schema) -> rusqlite::Result<Vec<String>> {
    if let Some(tables) = BASE_TABLES.with(|cache| cache.borrow().get(schema.sql).cloned()) {
        return Ok(tables);
    }
    let base = Connection::open_in_memory()?;
    base.execute_batch(schema.sql)?;
    let tables = base.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    BASE_TABLES.with(|cache| cache.borrow_mut().insert(schema.sql, tables.clone()));
    Ok(tables)
}

fn object_exists(db: &Connection, kind: &str, name: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type=?1 AND name=?2 COLLATE NOCASE)",
        [kind, name],
        |row| row.get(0),
    )
}

/// What a store's `user_version` says about the path to the current base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoreState {
    /// No schema yet; a writable open creates the base.
    Fresh,
    /// The current base is present.
    Current,
    /// An older base with a declared migration path to the current one.
    Upgrade(i64),
}

/// The one-shot migrations from `found` to the current base, in order.
fn upgrade_path(schema: Schema, found: i64) -> Result<Vec<&'static Migration>, VersionError> {
    let migrations: &'static [Migration] = schema.migrations;
    let mut path = Vec::new();
    let mut version = found;
    while version != schema.version {
        let Some(migration) = migrations
            .iter()
            .find(|migration| migration.from == version)
        else {
            return Err(VersionError::Refused {
                found,
                expected: schema.version,
            });
        };
        if migration.to <= migration.from || path.len() >= migrations.len() {
            return Err(VersionError::InvalidMigration {
                from: migration.from,
                reason: "a migration must advance without looping",
            });
        }
        version = migration.to;
        path.push(migration);
    }
    Ok(path)
}

/// Detect freshness and validate known base tables without applying anything.
/// A read-only open refuses an older base because only a writable open migrates.
fn inspect(db: &Connection, schema: Schema, writable: bool) -> Result<StoreState, VersionError> {
    let (version, has_schema): (i64, bool) = db.query_row(
        "SELECT (SELECT user_version FROM pragma_user_version), EXISTS(SELECT 1 FROM sqlite_schema)",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if version == 0 && !has_schema {
        return Ok(StoreState::Fresh);
    }
    let state = if version == schema.version {
        StoreState::Current
    } else if writable {
        upgrade_path(schema, version)?;
        StoreState::Upgrade(version)
    } else {
        // An older base with a declared path needs a writable open; any other
        // version is refused exactly as before.
        upgrade_path(schema, version)?;
        return Err(VersionError::MigrationRequired {
            found: version,
            expected: schema.version,
        });
    };
    for table in base_tables(schema)? {
        if !object_exists(db, "table", &table)? {
            return Err(VersionError::MissingBaseTable { table });
        }
    }
    Ok(state)
}

/// Writable-open preflight, before changing journal mode. Checks only the
/// format and base; `ensure_schema` applies any migrations and missing steps.
pub fn check_format(db: &Connection, schema: Schema) -> Result<(), VersionError> {
    inspect(db, schema, true).map(drop)
}

/// Read-only validation: the store must have its current base and every known
/// step. Unknown same-major objects are ignored; only a writable open creates a
/// base or applies a migration.
pub fn check_version(db: &Connection, schema: Schema) -> Result<(), VersionError> {
    match inspect(db, schema, false)? {
        StoreState::Fresh => {
            return Err(VersionError::Refused {
                found: 0,
                expected: schema.version,
            });
        }
        StoreState::Current => {}
        StoreState::Upgrade(found) => {
            return Err(VersionError::MigrationRequired {
                found,
                expected: schema.version,
            });
        }
    }
    for step in schema.steps {
        if step.missing(db)? {
            return Err(VersionError::MissingStep {
                minor: step.minor,
                object: step.object(),
            });
        }
    }
    Ok(())
}

pub fn stored_version(db: &Connection) -> rusqlite::Result<i64> {
    db.query_row("SELECT user_version FROM pragma_user_version", [], |row| {
        row.get(0)
    })
}

impl Step {
    fn object(self) -> String {
        match self.kind {
            StepKind::Table { name, .. } | StepKind::Index { name, .. } => name.to_owned(),
            StepKind::Column { table, column, .. } => format!("{table}.{column}"),
        }
    }

    fn missing(self, db: &Connection) -> rusqlite::Result<bool> {
        Ok(!match self.kind {
            StepKind::Table { name, .. } => object_exists(db, "table", name)?,
            StepKind::Index { name, .. } => object_exists(db, "index", name)?,
            StepKind::Column { table, column, .. } => db.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2 COLLATE NOCASE)",
                [table, column],
                |row| row.get(0),
            )?,
        })
    }

    fn invalid(self, reason: &'static str) -> VersionError {
        VersionError::InvalidStep {
            minor: self.minor,
            reason,
        }
    }

    fn apply(self, db: &Connection) -> Result<(), VersionError> {
        match self.kind {
            StepKind::Table { sql, .. } | StepKind::Index { sql, .. } => {
                let words: Vec<_> = sql
                    .split_whitespace()
                    .map(str::to_ascii_uppercase)
                    .collect();
                let prefix: &[&str] = match self.kind {
                    StepKind::Table { .. } => &["CREATE", "TABLE", "IF", "NOT", "EXISTS"],
                    _ if words.get(1).is_some_and(|word| word == "UNIQUE") => {
                        &["CREATE", "UNIQUE", "INDEX", "IF", "NOT", "EXISTS"]
                    }
                    _ => &["CREATE", "INDEX", "IF", "NOT", "EXISTS"],
                };
                if !words
                    .iter()
                    .map(String::as_str)
                    .take(prefix.len())
                    .eq(prefix.iter().copied())
                {
                    return Err(
                        self.invalid("table and index SQL must use CREATE ... IF NOT EXISTS")
                    );
                }
                db.execute(sql, [])?;
            }
            StepKind::Column {
                table,
                column,
                decl,
            } => {
                db.execute(
                    &format!(
                        "ALTER TABLE {} ADD COLUMN {} {decl}",
                        quote(table),
                        quote(column)
                    ),
                    [],
                )?;
                let (not_null, default): (bool, Option<String>) = db.query_row(
                    "SELECT \"notnull\", dflt_value FROM pragma_table_info(?1) WHERE name=?2 COLLATE NOCASE",
                    [table, column], |row| Ok((row.get(0)?, row.get(1)?)))?;
                if not_null {
                    let invalid =
                        || self.invalid("NOT NULL columns require a non-NULL constant DEFAULT");
                    let default = default.ok_or_else(invalid)?;
                    // Ask SQLite to validate this default on a populated table.
                    // Empty tables otherwise permit NOT NULL without a usable
                    // default. No second SQL-expression grammar is needed.
                    let probe = Connection::open_in_memory()?;
                    probe.execute_batch(
                        "CREATE TABLE probe (id INTEGER); INSERT INTO probe (id) VALUES (1)",
                    )?;
                    probe
                        .execute(
                            &format!(
                                "ALTER TABLE probe ADD COLUMN value NOT NULL DEFAULT {default}"
                            ),
                            [],
                        )
                        .map_err(|_| invalid())?;
                }
            }
        }
        if self.missing(db)? {
            return Err(self.invalid("SQL did not create the declared object"));
        }
        Ok(())
    }
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// Create the base, apply one-shot migrations to an older base, and add missing
/// steps atomically. A fully current store takes no write lock. Concurrent
/// openers re-check after acquiring the writer lock.
pub fn ensure_schema(db: &Connection, schema: Schema) -> Result<(), VersionError> {
    let state = inspect(db, schema, true)?;
    if state == StoreState::Current {
        let mut missing = false;
        for step in schema.steps {
            missing |= step.missing(db)?;
        }
        if !missing {
            return Ok(());
        }
    }
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    match inspect(&tx, schema, true)? {
        StoreState::Fresh => tx.execute_batch(schema.sql)?,
        StoreState::Current => {}
        StoreState::Upgrade(found) => {
            for migration in upgrade_path(schema, found)? {
                tx.execute_batch(migration.sql)?;
                tx.pragma_update(None, "user_version", migration.to)?;
            }
        }
    }
    for step in schema.steps {
        if step.missing(&tx)? {
            step.apply(&tx)?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Declaration guard used by owner-schema tests. Exercise the actual SQLite
/// parser and the same step application used for writable opens.
pub fn validate_steps(schema: Schema) -> Result<(), VersionError> {
    if schema
        .steps
        .windows(2)
        .any(|steps| steps[0].minor > steps[1].minor)
    {
        return Err(VersionError::InvalidStep {
            minor: 0,
            reason: "steps must be ordered by release minor",
        });
    }
    for (index, migration) in schema.migrations.iter().enumerate() {
        if migration.from >= migration.to {
            return Err(VersionError::InvalidMigration {
                from: migration.from,
                reason: "a migration must advance to a later version",
            });
        }
        if migration.to > schema.version {
            return Err(VersionError::InvalidMigration {
                from: migration.from,
                reason: "a migration target is newer than the base",
            });
        }
        if schema.migrations[..index]
            .iter()
            .any(|prior| prior.from == migration.from)
        {
            return Err(VersionError::InvalidMigration {
                from: migration.from,
                reason: "migration source versions must be unique",
            });
        }
    }
    for migration in schema.migrations {
        upgrade_path(schema, migration.from)?;
    }
    let db = Connection::open_in_memory()?;
    db.execute_batch(schema.sql)?;
    // A writable open runs migrations before steps. Validating them against
    // empty rows exercises the same order and SQL.
    for migration in schema.migrations {
        db.execute_batch(migration.sql)?;
    }
    // Validate every declaration, even if a table/index already exists in the
    // base. Existence may skip work at open, never declaration validation.
    for step in schema.steps {
        step.apply(&db)?;
    }
    check_version(&db, schema)
}

/// Exact declared base + steps, including altered table SQL. Opening accepts
/// unknown same-major objects, but this diagnostic rejects undeclared objects.
pub fn schema_matches(db: &Connection, schema: Schema) -> Result<bool, VersionError> {
    fn objects(db: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
        db.prepare("SELECT type, name, sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY type, name")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect()
    }
    let expected = Connection::open_in_memory()?;
    ensure_schema(&expected, schema)?;
    Ok(objects(db)? == objects(&expected)?)
}
