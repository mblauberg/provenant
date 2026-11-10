CREATE TABLE operations (
 project TEXT NOT NULL, actor TEXT NOT NULL, operation TEXT NOT NULL,
 digest TEXT NOT NULL, run TEXT NOT NULL UNIQUE, record BLOB NOT NULL CHECK(length(record)<=1048576),
 PRIMARY KEY(project, actor, operation)
) STRICT;
CREATE TABLE cancellations (
 project TEXT NOT NULL, actor TEXT NOT NULL, operation TEXT NOT NULL, target TEXT NOT NULL,
 PRIMARY KEY(project, actor, operation)
) STRICT;
CREATE TABLE admission_windows (
 project TEXT NOT NULL, actor TEXT NOT NULL, operation TEXT NOT NULL,
 deadline INTEGER NOT NULL, wake_at INTEGER,
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
 record BLOB NOT NULL,
 tmp_retained INTEGER NOT NULL DEFAULT 1 CHECK(tmp_retained IN (0,1)), UNIQUE(run,task,number)
) STRICT;
CREATE INDEX terminal_attempts ON attempts(project,terminal,sequence);
CREATE INDEX retained_temporary ON attempts(project,run,sequence) WHERE terminal=1 AND tmp_retained=1;
CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,run,project,task,number,revision,terminal,record ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
CREATE TRIGGER immutable_terminal_delete BEFORE DELETE ON attempts WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
CREATE TABLE admission (
 attempt TEXT PRIMARY KEY REFERENCES attempts(id), project TEXT NOT NULL, run TEXT NOT NULL,
 active INTEGER NOT NULL CHECK(active IN (0,1)), record BLOB NOT NULL CHECK(length(record)<=1048576)
) STRICT;
CREATE INDEX admission_counts ON admission(active,project,run);
CREATE TABLE continuations (
 target TEXT PRIMARY KEY REFERENCES attempts(id), run TEXT NOT NULL REFERENCES runs(id)
) STRICT;
CREATE INDEX continuation_runs ON continuations(run);
CREATE TABLE reactions (
 attempt TEXT PRIMARY KEY REFERENCES attempts(id),
 run TEXT NOT NULL REFERENCES runs(id),
 project TEXT NOT NULL,
 settled_at INTEGER NOT NULL,
 pending INTEGER NOT NULL CHECK(pending IN (0,1)),
 payload BLOB NOT NULL
) STRICT;
CREATE INDEX reaction_pending ON reactions(run) WHERE pending=1;
CREATE TABLE settlement_receipts (
 attempt TEXT PRIMARY KEY REFERENCES attempts(id),
 generation INTEGER NOT NULL,
 record BLOB NOT NULL CHECK(length(record)<=1048576)
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
PRAGMA user_version=10;
