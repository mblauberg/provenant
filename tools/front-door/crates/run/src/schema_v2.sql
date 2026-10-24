-- Version 2: cancel operations are bound to their target.
CREATE TABLE IF NOT EXISTS cancellations (
 project TEXT NOT NULL, actor TEXT NOT NULL, operation TEXT NOT NULL, target TEXT NOT NULL,
 PRIMARY KEY(project, actor, operation)
) STRICT;
PRAGMA user_version=2;
