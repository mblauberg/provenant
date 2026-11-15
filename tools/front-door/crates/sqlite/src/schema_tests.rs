use crate::{
    Schema, Step, StepKind, VersionError, check_version, ensure_schema, schema_matches,
    stored_version, validate_steps,
};
use rusqlite::Connection;

const BASE: Schema = Schema {
    sql: "CREATE TABLE item (id INTEGER PRIMARY KEY, value TEXT NOT NULL) STRICT; PRAGMA user_version=3;",
    version: 3,
    steps: &[],
};
const NEWER: Schema = Schema {
    steps: &[
        Step {
            minor: 1,
            kind: StepKind::Table {
                name: "extra",
                sql: "CREATE TABLE IF NOT EXISTS extra (id INTEGER PRIMARY KEY) STRICT",
            },
        },
        Step {
            minor: 1,
            kind: StepKind::Column {
                table: "item",
                column: "note",
                decl: "TEXT",
            },
        },
        Step {
            minor: 2,
            kind: StepKind::Index {
                name: "item_note",
                sql: "CREATE INDEX IF NOT EXISTS item_note ON item(note)",
            },
        },
        Step {
            minor: 2,
            kind: StepKind::Column {
                table: "item",
                column: "priority",
                decl: "INTEGER NOT NULL DEFAULT 0",
            },
        },
    ],
    ..BASE
};

fn catalogue(db: &Connection) -> rusqlite::Result<Vec<(String, String, Option<String>)>> {
    db.prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect()
}

#[test]
fn same_major_additions_are_bidirectionally_compatible_and_idempotent()
-> Result<(), Box<dyn std::error::Error>> {
    let db = Connection::open_in_memory()?;
    ensure_schema(&db, BASE)?;
    db.execute("INSERT INTO item (value) VALUES ('before')", [])?;
    let before = catalogue(&db)?;
    assert!(matches!(
        check_version(&db, NEWER),
        Err(VersionError::MissingStep { .. })
    ));
    assert_eq!(catalogue(&db)?, before);
    ensure_schema(&db, NEWER)?;
    assert_eq!(stored_version(&db)?, BASE.version);
    assert!(schema_matches(&db, NEWER)?);
    let after = catalogue(&db)?;
    ensure_schema(&db, NEWER)?;
    assert_eq!(catalogue(&db)?, after);
    check_version(&db, BASE)?;
    ensure_schema(&db, BASE)?;
    db.execute("INSERT INTO item (value) VALUES ('older writer')", [])?;
    let value: String = db.query_row(
        "SELECT value FROM item WHERE value='older writer'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(value, "older writer");
    let defaults: (Option<String>, i64) = db.query_row(
        "SELECT note, priority FROM item WHERE value='older writer'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(defaults, (None, 0));
    assert_eq!(catalogue(&db)?, after);
    assert!(!schema_matches(&db, BASE)?);
    let fresh = Connection::open_in_memory()?;
    ensure_schema(&fresh, NEWER)?;
    assert!(schema_matches(&fresh, NEWER)?);
    // Exactness still catches undeclared changes to the base.
    fresh.execute("DROP INDEX item_note", [])?;
    assert!(!schema_matches(&fresh, NEWER)?);
    Ok(())
}

#[test]
fn current_opens_do_not_take_a_write_lock_and_concurrent_upgrades_recheck()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = std::env::temp_dir().canonicalize()?.join(format!(
        "provenant-sqlite-steps-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&temp)?;
    let path = temp.join("store.sqlite3");
    let db = Connection::open(&path)?;
    ensure_schema(&db, BASE)?;
    let reader = crate::open_connection(&path, true)?;
    assert!(matches!(
        check_version(&reader, NEWER),
        Err(VersionError::MissingStep { .. })
    ));
    drop(reader);
    let start = std::sync::Barrier::new(2);
    let first_db = crate::open_connection(&path, false)?;
    let second_db = crate::open_connection(&path, false)?;
    std::thread::scope(|scope| {
        let start = &start;
        let open = move |db: Connection| -> Result<(), VersionError> {
            start.wait();
            ensure_schema(&db, NEWER)
        };
        let first = scope.spawn(move || open(first_db));
        let second = scope.spawn(move || open(second_db));
        first.join().unwrap()?;
        second.join().unwrap()?;
        Ok::<_, VersionError>(())
    })?;
    assert!(schema_matches(&db, NEWER)?);
    let before = catalogue(&db)?;
    let holder = crate::open_connection(&path, false)?;
    holder.execute_batch("BEGIN IMMEDIATE")?;
    db.busy_timeout(std::time::Duration::ZERO)?;
    ensure_schema(&db, NEWER)?;
    ensure_schema(&db, BASE)?;
    let reader = crate::open_connection(&path, true)?;
    check_version(&reader, NEWER)?;
    ensure_schema(&reader, NEWER)?;
    drop(reader);
    assert_eq!(catalogue(&db)?, before);
    holder.execute_batch("ROLLBACK")?;
    drop(holder);
    drop(db);
    std::fs::remove_dir_all(temp)?;
    Ok(())
}

#[test]
fn incompatible_stores_are_refused_byte_unchanged() -> Result<(), Box<dyn std::error::Error>> {
    let temp = std::env::temp_dir().canonicalize()?.join(format!(
        "provenant-sqlite-version-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&temp)?;
    for (name, version) in [
        ("older", NEWER.version - 1),
        ("newer", NEWER.version + 1),
        ("unversioned", 0),
    ] {
        let path = temp.join(name);
        let db = Connection::open(&path)?;
        ensure_schema(&db, NEWER)?;
        db.pragma_update(None, "user_version", version)?;
        drop(db);
        let before = std::fs::read(&path)?;
        let db = crate::open_connection(&path, false)?;
        for result in [check_version(&db, NEWER), ensure_schema(&db, NEWER)] {
            assert!(matches!(result, Err(VersionError::Refused { found, .. }) if found == version));
        }
        drop(db);
        assert_eq!(std::fs::read(&path)?, before, "{name}");
    }
    std::fs::remove_dir_all(temp)?;
    Ok(())
}

#[test]
fn a_same_major_store_requires_known_base_tables() -> Result<(), Box<dyn std::error::Error>> {
    let db = Connection::open_in_memory()?;
    assert!(matches!(
        check_version(&db, BASE),
        Err(VersionError::Refused { .. })
    ));
    db.execute_batch("CREATE TABLE unrelated (id INTEGER); PRAGMA user_version=3")?;
    let before = catalogue(&db)?;
    assert!(matches!(
        check_version(&db, BASE),
        Err(VersionError::MissingBaseTable { .. })
    ));
    assert!(matches!(
        ensure_schema(&db, BASE),
        Err(VersionError::MissingBaseTable { .. })
    ));
    assert_eq!(catalogue(&db)?, before);
    Ok(())
}

// Include the same owner-local expressions production includes. This keeps
// declaration coverage without making sqlite depend on its own consumers.
use crate as provenant_sqlite;
const OWNERS: [Schema; 5] = [
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../fabric-store/src/schema.rs"
    )),
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../run/src/schema.rs")),
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../run/src/admission_schema.rs"
    )),
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../run/src/pr_contribution_schema.rs"
    )),
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../workflow/src/log_schema.rs"
    )),
];

#[test]
fn owner_step_declarations_are_additive() -> Result<(), Box<dyn std::error::Error>> {
    for schema in OWNERS.into_iter().chain([NEWER]) {
        validate_steps(schema)?;
    }
    const INVALID: [&[Step]; 4] = [
        &[Step {
            minor: 1,
            kind: StepKind::Table {
                name: "extra",
                sql: "CREATE TABLE extra (id INTEGER)",
            },
        }],
        &[Step {
            minor: 1,
            kind: StepKind::Index {
                name: "item_value",
                sql: "CREATE INDEX item_value ON item(value)",
            },
        }],
        &[Step {
            minor: 1,
            kind: StepKind::Column {
                table: "item",
                column: "required",
                decl: "TEXT NOT NULL",
            },
        }],
        &[Step {
            minor: 1,
            kind: StepKind::Column {
                table: "item",
                column: "required",
                decl: "TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP",
            },
        }],
    ];
    for steps in INVALID {
        assert!(validate_steps(Schema { steps, ..BASE }).is_err());
    }
    Ok(())
}

fn positional_inserts(
    source: &str,
    owners: &std::collections::HashSet<String>,
) -> Vec<(usize, String)> {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATTERN.get_or_init(|| regex::Regex::new(r#"(?ix)\bINSERT\s+(?:OR\s+\w+\s+)?INTO\s+(?:(?:\w+|\\?"[^"]+"|`[^`]+`|\[[^\]]+\])\s*\.\s*)?(\w+|\\?"[^"]+"|`[^`]+`|\[[^\]]+\])\s+(?:VALUES|SELECT)\b"#).unwrap());
    // Rust SQL literals can spell whitespace as escapes as well as newlines.
    let normalised = source
        .replace("\\n", "  ")
        .replace("\\r", "  ")
        .replace("\\t", "  ");
    pattern
        .captures_iter(&normalised)
        .filter_map(|capture| {
            let name = capture[1]
                .trim_matches(['\\', '"', '`', '[', ']'])
                .to_ascii_lowercase();
            owners.contains(&name).then(|| {
                let at = capture.get(0).unwrap().start();
                (
                    source[..at].bytes().filter(|byte| *byte == b'\n').count() + 1,
                    name,
                )
            })
        })
        .collect()
}

#[test]
fn owner_table_inserts_always_name_columns() -> Result<(), Box<dyn std::error::Error>> {
    let mut owners = std::collections::HashSet::new();
    for schema in OWNERS {
        // SQLite itself parses each owner's CREATE TABLE statements.
        let db = Connection::open_in_memory()?;
        ensure_schema(&db, schema)?;
        let mut query = db.prepare(
            "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        )?;
        for name in query.query_map([], |row| row.get::<_, String>(0))? {
            owners.insert(name?.to_ascii_lowercase());
        }
    }
    let front_door = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut pending = vec![front_door.join("crates"), front_door.join("src")];
    let mut violations = Vec::new();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            for entry in std::fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        } else if path
            .extension()
            .is_some_and(|ext| ext == "rs" || ext == "sql")
        {
            for (line, table) in positional_inserts(&std::fs::read_to_string(&path)?, &owners) {
                violations.push(format!("{}:{line}: {table}", path.display()));
            }
        }
    }
    violations.sort();
    assert!(
        violations.is_empty(),
        "positional owner INSERT statements:\n{}",
        violations.join("\n")
    );
    Ok(())
}

#[test]
fn insert_scanner_covers_sql_spelling_without_flagging_other_tables() {
    let owners = ["example_owner".to_owned()].into_iter().collect();
    for sql in [
        "insert\nor replace\ninto example_owner\nvalues (1)",
        "INSERT OR IGNORE INTO main.\"example_owner\" SELECT 1",
        "INSERT INTO [example_owner] VALUES (1)",
        "INSERT INTO `example_owner` SELECT 1",
        r"INSERT\nINTO example_owner\tVALUES (1)",
        r#"INSERT INTO \"example_owner\" VALUES (1)"#,
    ] {
        assert!(!positional_inserts(sql, &owners).is_empty(), "{sql}");
    }
    for sql in [
        "INSERT INTO example_owner (value) VALUES (1)",
        "INSERT INTO t VALUES (1)",
        "INSERT INTO session_message SELECT 1",
    ] {
        assert!(positional_inserts(sql, &owners).is_empty(), "{sql}");
    }
}
