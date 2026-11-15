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
    MissingBaseTable { table: String },
    MissingStep { minor: u32, object: String },
    InvalidStep { minor: u32, reason: &'static str },
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for VersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { found, expected } => write!(
                f,
                "{RESET_CODE}: store base version {found} is not {expected}; use a fresh state root"
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

/// Detect freshness and validate known base tables without applying any steps.
fn is_fresh(db: &Connection, schema: Schema) -> Result<bool, VersionError> {
    let (version, has_schema): (i64, bool) = db.query_row(
        "SELECT (SELECT user_version FROM pragma_user_version), EXISTS(SELECT 1 FROM sqlite_schema)",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if version == 0 && !has_schema {
        return Ok(true);
    }
    if version != schema.version {
        return Err(VersionError::Refused {
            found: version,
            expected: schema.version,
        });
    }
    for table in base_tables(schema)? {
        if !object_exists(db, "table", &table)? {
            return Err(VersionError::MissingBaseTable { table });
        }
    }
    Ok(false)
}

/// Writable-open preflight, before changing journal mode. Checks only the
/// format and base; `ensure_schema` applies any missing additive objects.
pub fn check_format(db: &Connection, schema: Schema) -> Result<(), VersionError> {
    is_fresh(db, schema).map(drop)
}

/// Read-only validation: the store must have its base and every known step.
/// Unknown same-major objects are ignored; only a writable open creates a base.
pub fn check_version(db: &Connection, schema: Schema) -> Result<(), VersionError> {
    if is_fresh(db, schema)? {
        return Err(VersionError::Refused {
            found: 0,
            expected: schema.version,
        });
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

/// Create the base and missing steps atomically. A fully current store takes
/// no write lock. Concurrent openers re-check after acquiring the writer lock.
pub fn ensure_schema(db: &Connection, schema: Schema) -> Result<(), VersionError> {
    let fresh = is_fresh(db, schema)?;
    if !fresh {
        let mut missing = false;
        for step in schema.steps {
            missing |= step.missing(db)?;
        }
        if !missing {
            return Ok(());
        }
    }
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    if is_fresh(&tx, schema)? {
        tx.execute_batch(schema.sql)?;
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
    let db = Connection::open_in_memory()?;
    db.execute_batch(schema.sql)?;
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
