CREATE TABLE operations (
 project TEXT NOT NULL, actor TEXT NOT NULL, operation TEXT NOT NULL,
 digest TEXT NOT NULL, run TEXT NOT NULL UNIQUE, record BLOB NOT NULL CHECK(length(record)<=1048576),
 PRIMARY KEY(project, actor, operation)
) STRICT;
CREATE TABLE runs (
 id TEXT PRIMARY KEY, project TEXT NOT NULL, record BLOB NOT NULL CHECK(length(record)<=1048576),
 FOREIGN KEY(id) REFERENCES operations(run)
) STRICT;
CREATE TABLE attempts (
 sequence INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE, run TEXT NOT NULL REFERENCES runs(id),
 project TEXT NOT NULL, task TEXT NOT NULL, number INTEGER NOT NULL CHECK(number>0),
 revision INTEGER NOT NULL, terminal INTEGER NOT NULL CHECK(terminal IN (0,1)),
 record BLOB NOT NULL CHECK(length(record)<=1048576), UNIQUE(run,task,number)
) STRICT;
CREATE INDEX terminal_attempts ON attempts(project,terminal,sequence);
CREATE TRIGGER immutable_terminal_update BEFORE UPDATE ON attempts WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
CREATE TRIGGER immutable_terminal_delete BEFORE DELETE ON attempts WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
CREATE TABLE admission (
 attempt TEXT PRIMARY KEY REFERENCES attempts(id), project TEXT NOT NULL, run TEXT NOT NULL,
 active INTEGER NOT NULL CHECK(active IN (0,1)), record BLOB NOT NULL CHECK(length(record)<=1048576)
) STRICT;
CREATE INDEX admission_counts ON admission(active,project,run);
CREATE TABLE continuations (
 target TEXT PRIMARY KEY REFERENCES attempts(id), run TEXT NOT NULL UNIQUE REFERENCES runs(id)
) STRICT;
CREATE TABLE events (
 sequence INTEGER PRIMARY KEY AUTOINCREMENT, project TEXT NOT NULL,
 attempt TEXT NOT NULL REFERENCES attempts(id), revision INTEGER NOT NULL,
 record BLOB NOT NULL CHECK(length(record)<=1048576), UNIQUE(attempt,revision)
) STRICT;
CREATE TABLE cursors (
 project TEXT NOT NULL, actor TEXT NOT NULL, selection TEXT NOT NULL,
 sequence INTEGER NOT NULL CHECK(sequence>=0), PRIMARY KEY(project,actor,selection)
) STRICT;
PRAGMA user_version=1;
