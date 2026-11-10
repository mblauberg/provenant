CREATE TABLE meta (id INTEGER PRIMARY KEY CHECK(id=1), total_appended INTEGER NOT NULL, total_pruned INTEGER NOT NULL, expiry_cutoff INTEGER NOT NULL);
INSERT INTO meta VALUES (1, 0, 0, -9223372036854775808);
CREATE TABLE entries (sequence INTEGER PRIMARY KEY, created_at INTEGER NOT NULL, bytes INTEGER NOT NULL, payload BLOB NOT NULL);
CREATE INDEX entries_age ON entries(created_at);
CREATE TABLE receipts (operation_id TEXT PRIMARY KEY, issued_at INTEGER NOT NULL, digest TEXT NOT NULL, sequence INTEGER NOT NULL, created_at INTEGER NOT NULL, host TEXT NOT NULL);
CREATE INDEX receipts_age ON receipts(issued_at);
PRAGMA user_version=1;
