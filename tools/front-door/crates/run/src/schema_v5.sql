-- Version 5: a run may hold several continuation rows. A policy retry chain
-- continues each failed attempt within its own run, and a resumed run that
-- retries continues its source and then its own attempts. Each target is
-- still continued at most once.
CREATE TABLE continuations_v5 (
 target TEXT PRIMARY KEY REFERENCES attempts(id), run TEXT NOT NULL REFERENCES runs(id)
) STRICT;
INSERT INTO continuations_v5(target,run) SELECT target,run FROM continuations;
DROP TABLE continuations;
ALTER TABLE continuations_v5 RENAME TO continuations;
CREATE INDEX continuation_runs ON continuations(run);
PRAGMA user_version=5;
