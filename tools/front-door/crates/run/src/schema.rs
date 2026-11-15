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
        provenant_sqlite::Step {
            minor: 3,
            kind: provenant_sqlite::StepKind::Table {
                name: "steer_continuations",
                sql: "CREATE TABLE IF NOT EXISTS steer_continuations (attempt TEXT PRIMARY KEY REFERENCES attempts(id), state TEXT NOT NULL CHECK(state IN ('admitted','resumed','not_continued')), cause TEXT CHECK(cause IN ('session_unavailable','session_mismatch','session_proof_incomplete','terminal_stop','continuation_refused')), successor TEXT, live_ack INTEGER NOT NULL DEFAULT 0 CHECK(live_ack IN (0,1)), terminal_final INTEGER NOT NULL DEFAULT 0 CHECK(terminal_final IN (0,1)), CHECK((state='admitted' AND cause IS NULL AND successor IS NOT NULL AND live_ack=0 AND terminal_final=0) OR (state='resumed' AND cause IS NULL AND successor IS NOT NULL AND (live_ack=1 OR terminal_final=1)) OR (state='not_continued' AND cause IS NOT NULL AND (successor IS NOT NULL OR terminal_final=1)))) STRICT",
            },
        },
        provenant_sqlite::Step {
            minor: 3,
            kind: provenant_sqlite::StepKind::Table {
                name: "continuation_expectations",
                sql: "CREATE TABLE IF NOT EXISTS continuation_expectations (attempt TEXT PRIMARY KEY REFERENCES attempts(id), session TEXT NOT NULL CHECK(length(session)>0 AND length(session)<=4096), route_digest TEXT NOT NULL CHECK(length(route_digest)=64)) STRICT",
            },
        },
    ],
}
