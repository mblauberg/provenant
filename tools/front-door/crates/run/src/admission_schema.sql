CREATE TABLE leases (
 attempt TEXT PRIMARY KEY, project TEXT NOT NULL, store BLOB NOT NULL,
 class TEXT NOT NULL DEFAULT 'writer' CHECK(class IN ('writer','read_only')),
 boot TEXT NOT NULL, guardian TEXT NOT NULL, owner_id TEXT NOT NULL,
 yielded INTEGER NOT NULL DEFAULT 0 CHECK(yielded IN (0,1))
) STRICT;
CREATE INDEX lease_projects ON leases(project);
CREATE TABLE spacing (
 class TEXT PRIMARY KEY CHECK(class IN ('writer','read_only')),
 boot TEXT NOT NULL,
 last_start INTEGER NOT NULL CHECK(last_start>=0)
) STRICT;
PRAGMA user_version=6;
