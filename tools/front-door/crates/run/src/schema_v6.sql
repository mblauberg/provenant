-- Version 6: reactions a settled attempt still owes. A row is inserted in the
-- transaction that commits the attempt's terminal record, so a crash between
-- commit and reaction strands nothing. Only attempts that settle after the
-- upgrade get rows.
CREATE TABLE reactions (
 attempt TEXT PRIMARY KEY REFERENCES attempts(id),
 run TEXT NOT NULL REFERENCES runs(id),
 project TEXT NOT NULL,
 settled_at INTEGER NOT NULL,
 pending INTEGER NOT NULL CHECK(pending IN (0,1)),
 payload BLOB NOT NULL
) STRICT;
CREATE INDEX reaction_pending ON reactions(run) WHERE pending=1;
PRAGMA user_version=6;
