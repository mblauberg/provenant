-- Payloads and paths can be pruned; scope identity/generation never resets.
CREATE TABLE claim_counters (
 project TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('work','landing')),
 scope_key TEXT NOT NULL, id TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation >= 1),
 PRIMARY KEY(project,kind,scope_key), UNIQUE(project,id)
) STRICT;
INSERT INTO claim_counters SELECT project,kind,scope_key,id,generation FROM claims;
CREATE INDEX claim_path_lookup ON claim_paths(project,path,claim);
CREATE INDEX released_claims ON claims(project,id) WHERE holder IS NULL;
