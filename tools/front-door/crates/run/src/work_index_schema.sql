CREATE TABLE membership (
 project TEXT NOT NULL, run TEXT NOT NULL, task TEXT NOT NULL,
 attempt TEXT PRIMARY KEY, record BLOB NOT NULL
) STRICT;
CREATE INDEX work_runs ON membership(project,run,task,attempt);
PRAGMA user_version=1;
