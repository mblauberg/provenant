-- F6a/F6b. Unknown peer operations cannot be replayed by preparation alone.
CREATE TABLE peer_operations (
 project TEXT NOT NULL, id TEXT NOT NULL, actor TEXT NOT NULL,
 digest TEXT NOT NULL,
 phase TEXT NOT NULL CHECK(phase IN ('prepared','sent','unknown','committed')),
 request BLOB NOT NULL, response BLOB,
 PRIMARY KEY(project,id),
 CHECK((phase='committed' AND response IS NOT NULL)
    OR (phase<>'committed' AND response IS NULL))
) STRICT;

-- Cursor advancement commits with the imported rows in the shared project feed.
CREATE TABLE run_event_cursors (
 project TEXT NOT NULL, host TEXT NOT NULL,
 sequence INTEGER NOT NULL CHECK(sequence >= 0),
 PRIMARY KEY(project,host)
) STRICT;
