-- Preserve existing rows and references while raising the bounded attempt ledger.
CREATE TABLE attempts_v9 (
 sequence INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE, run TEXT NOT NULL REFERENCES runs(id),
 project TEXT NOT NULL, task TEXT NOT NULL, number INTEGER NOT NULL CHECK(number>0),
 revision INTEGER NOT NULL, terminal INTEGER NOT NULL CHECK(terminal IN (0,1)),
 record BLOB NOT NULL CHECK(length(record)<=16777216),
 tmp_retained INTEGER NOT NULL DEFAULT 1 CHECK(tmp_retained IN (0,1)), UNIQUE(run,task,number)
) STRICT;
INSERT INTO attempts_v9 SELECT sequence,id,run,project,task,number,revision,terminal,record,tmp_retained FROM attempts;
DROP TABLE attempts;
ALTER TABLE attempts_v9 RENAME TO attempts;
CREATE INDEX terminal_attempts ON attempts(project,terminal,sequence);
CREATE INDEX retained_temporary ON attempts(project,run,sequence) WHERE terminal=1 AND tmp_retained=1;
CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,run,project,task,number,revision,terminal,record ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
CREATE TRIGGER immutable_terminal_delete BEFORE DELETE ON attempts WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
PRAGMA user_version=9;
