-- Persistent scope counters survive release and expiry. Tokens never enter records/events.
CREATE TABLE claims (
 project TEXT NOT NULL, id TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('work','landing')),
 scope_key TEXT NOT NULL, generation INTEGER NOT NULL CHECK(generation >= 1),
 holder TEXT, token BLOB, expires INTEGER, record BLOB NOT NULL,
 PRIMARY KEY(project,id), UNIQUE(project,kind,scope_key),
 CHECK((holder IS NULL AND token IS NULL AND expires IS NULL)
    OR (holder IS NOT NULL AND length(token)=16 AND expires IS NOT NULL))
) STRICT;
CREATE TABLE claim_paths (
 project TEXT NOT NULL, claim TEXT NOT NULL, path TEXT NOT NULL,
 PRIMARY KEY(project,claim,path),
 FOREIGN KEY(project,claim) REFERENCES claims(project,id)
) STRICT;
CREATE TABLE operations (
 project TEXT NOT NULL, id TEXT NOT NULL, claim TEXT NOT NULL,
 digest TEXT NOT NULL, phase TEXT NOT NULL CHECK(phase IN ('prepared','pushing','unknown','committed','retryable','conflict')),
 record BLOB NOT NULL, PRIMARY KEY(project,id),
 FOREIGN KEY(project,claim) REFERENCES claims(project,id)
) STRICT;
CREATE INDEX claim_operations ON operations(project,claim,phase);
