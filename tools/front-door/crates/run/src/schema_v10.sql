-- Diagnostic ledgers are host-owned evidence, not an admission limit.
-- Preserve references, terminal immutability and retained-temporary indexes.
CREATE TABLE attempts_v10 (
 sequence INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE, run TEXT NOT NULL REFERENCES runs(id),
 project TEXT NOT NULL, task TEXT NOT NULL, number INTEGER NOT NULL CHECK(number>0),
 revision INTEGER NOT NULL, terminal INTEGER NOT NULL CHECK(terminal IN (0,1)),
 record BLOB NOT NULL,
 tmp_retained INTEGER NOT NULL DEFAULT 1 CHECK(tmp_retained IN (0,1)), UNIQUE(run,task,number)
) STRICT;
INSERT INTO attempts_v10 SELECT sequence,id,run,project,task,number,revision,terminal,record,tmp_retained FROM attempts;
DROP TABLE attempts;
ALTER TABLE attempts_v10 RENAME TO attempts;
CREATE INDEX terminal_attempts ON attempts(project,terminal,sequence);
CREATE INDEX retained_temporary ON attempts(project,run,sequence) WHERE terminal=1 AND tmp_retained=1;
CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,run,project,task,number,revision,terminal,record ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
CREATE TRIGGER immutable_terminal_delete BEFORE DELETE ON attempts WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
PRAGMA user_version=10;
