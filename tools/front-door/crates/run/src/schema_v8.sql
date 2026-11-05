-- Version 8: CLI admission windows under the operation key.
CREATE TABLE IF NOT EXISTS admission_windows (
 project TEXT NOT NULL, actor TEXT NOT NULL, operation TEXT NOT NULL,
 deadline INTEGER NOT NULL, wake_at INTEGER,
 PRIMARY KEY(project, actor, operation)
) STRICT;
PRAGMA user_version=8;
