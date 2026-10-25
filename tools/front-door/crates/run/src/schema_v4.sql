-- Version 4: retry terminal temporary removal without scanning settled history.
-- Default retained also covers older connections committing in-flight attempts.
ALTER TABLE attempts ADD COLUMN tmp_retained INTEGER NOT NULL DEFAULT 1 CHECK(tmp_retained IN (0,1));
CREATE INDEX retained_temporary ON attempts(project,run,sequence) WHERE terminal=1 AND tmp_retained=1;
DROP TRIGGER immutable_terminal_update;
CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,run,project,task,number,revision,terminal,record ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
PRAGMA user_version=4;
