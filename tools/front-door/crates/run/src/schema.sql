CREATE TABLE attempts (
 sequence INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
 project TEXT NOT NULL, graph TEXT NOT NULL, node TEXT NOT NULL,
 attempt INTEGER NOT NULL CHECK(attempt>0), digest TEXT NOT NULL,
 actor TEXT NOT NULL, operation TEXT NOT NULL,
 number INTEGER NOT NULL CHECK(number>0), continues TEXT UNIQUE REFERENCES attempts(id),
 refused INTEGER NOT NULL DEFAULT 0 CHECK(refused IN (0,1)),
 operation_record BLOB NOT NULL CHECK(length(operation_record)<=1048576),
 revision INTEGER NOT NULL, terminal INTEGER NOT NULL CHECK(terminal IN (0,1)),
 record BLOB,
 sealed INTEGER NOT NULL DEFAULT 0 CHECK(sealed IN (0,1)), repository TEXT, head TEXT,
 tmp_retained INTEGER NOT NULL DEFAULT 1 CHECK(tmp_retained IN (0,1)),
 UNIQUE(project,graph,node,attempt),
 CHECK((refused=1 AND terminal=1 AND record IS NULL) OR (refused=0 AND record IS NOT NULL))
) STRICT;
CREATE INDEX attempt_operations ON attempts(project,actor,operation);
CREATE INDEX terminal_attempts ON attempts(project,terminal,sequence);
CREATE INDEX retained_temporary ON attempts(project,sequence) WHERE terminal=1 AND tmp_retained=1;
CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
CREATE TRIGGER immutable_terminal_delete BEFORE DELETE ON attempts WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
CREATE TABLE admission (
 attempt TEXT PRIMARY KEY REFERENCES attempts(id), project TEXT NOT NULL,
 active INTEGER NOT NULL CHECK(active IN (0,1)), record BLOB NOT NULL CHECK(length(record)<=1048576)
) STRICT;
CREATE INDEX admission_counts ON admission(active,project);
CREATE TABLE reactions (
 attempt TEXT PRIMARY KEY REFERENCES attempts(id),
 project TEXT NOT NULL,
 settled_at INTEGER NOT NULL,
 pending INTEGER NOT NULL CHECK(pending IN (0,1)),
 payload BLOB NOT NULL
) STRICT;
CREATE INDEX reaction_pending ON reactions(project) WHERE pending=1;
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
PRAGMA user_version=12;
