provenant_sqlite::Schema {
    sql: include_str!("schema.sql"),
    version: 12,
    steps: &[
        provenant_sqlite::Step {
            minor: 3,
            kind: provenant_sqlite::StepKind::Table {
                name: "steer_intents",
                sql: "CREATE TABLE IF NOT EXISTS steer_intents (sequence INTEGER PRIMARY KEY AUTOINCREMENT, attempt TEXT NOT NULL REFERENCES attempts(id), operation TEXT NOT NULL UNIQUE, digest TEXT NOT NULL, record BLOB NOT NULL CHECK(length(record)<=32768), resolved INTEGER NOT NULL DEFAULT 0 CHECK(resolved IN (0,1))) STRICT",
            },
        },
        provenant_sqlite::Step {
            minor: 3,
            kind: provenant_sqlite::StepKind::Index {
                name: "steer_attempts",
                sql: "CREATE INDEX IF NOT EXISTS steer_attempts ON steer_intents(attempt,sequence)",
            },
        },
        provenant_sqlite::Step {
            minor: 3,
            kind: provenant_sqlite::StepKind::Table {
                name: "provider_session_ready",
                sql: "CREATE TABLE IF NOT EXISTS provider_session_ready (attempt TEXT PRIMARY KEY REFERENCES attempts(id), generation INTEGER NOT NULL CHECK(generation>0), route_digest TEXT NOT NULL CHECK(length(route_digest)=64), session TEXT CHECK(session IS NULL OR length(session)>0), provider_boot TEXT NOT NULL CHECK(length(provider_boot)>0), provider_pid INTEGER NOT NULL CHECK(provider_pid>0), provider_start INTEGER NOT NULL CHECK(provider_start>0), identity_conflict INTEGER NOT NULL DEFAULT 0 CHECK(identity_conflict IN (0,1)), terminal_confirmed INTEGER NOT NULL DEFAULT 0 CHECK(terminal_confirmed IN (0,1)), CHECK(session IS NOT NULL OR identity_conflict=1)) STRICT",
            },
        },
    ],
}
