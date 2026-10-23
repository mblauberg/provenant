-- F1, version 1. Claims/landing and identity add their own numbered migrations.
CREATE TABLE messages (
 project TEXT NOT NULL, id TEXT NOT NULL, record BLOB NOT NULL,
 PRIMARY KEY(project, id)
) STRICT;
CREATE TABLE delivery_sequences (
 project TEXT PRIMARY KEY, seq INTEGER NOT NULL CHECK(seq >= 0)
) STRICT;
CREATE TABLE deliveries (
 seq INTEGER NOT NULL CHECK(seq > 0),
 project TEXT NOT NULL, message TEXT NOT NULL, recipient TEXT NOT NULL,
 PRIMARY KEY(project, seq),
 UNIQUE(project, message, recipient),
 FOREIGN KEY(project, message) REFERENCES messages(project, id)
) STRICT;
CREATE INDEX inbox ON deliveries(project, recipient, seq);
CREATE TABLE mail_cursors (
 project TEXT NOT NULL, recipient TEXT NOT NULL, seq INTEGER NOT NULL CHECK(seq >= 0),
 PRIMARY KEY(project, recipient)
) STRICT;
CREATE TABLE tasks (
 project TEXT NOT NULL, id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('open','active','done','cancelled')),
 generation INTEGER NOT NULL CHECK(generation >= 0),
 owner TEXT, token BLOB, expires INTEGER, record BLOB NOT NULL,
 PRIMARY KEY(project, id),
 CHECK((state = 'active' AND owner IS NOT NULL AND length(token) = 16 AND expires IS NOT NULL)
    OR (state <> 'active' AND owner IS NULL AND token IS NULL AND expires IS NULL))
) STRICT;
CREATE TABLE events (
 seq INTEGER PRIMARY KEY AUTOINCREMENT, project TEXT NOT NULL,
 dedup TEXT, record BLOB NOT NULL, UNIQUE(project, dedup)
) STRICT;
CREATE INDEX event_cursor ON events(project, seq);
CREATE TABLE event_cursors (
 project TEXT NOT NULL, consumer TEXT NOT NULL, seq INTEGER NOT NULL CHECK(seq >= 0),
 PRIMARY KEY(project, consumer)
) STRICT;
CREATE TABLE imports (
 source TEXT PRIMARY KEY, project TEXT NOT NULL, legacy_project TEXT NOT NULL, imported_at INTEGER NOT NULL,
 tasks INTEGER NOT NULL, messages INTEGER NOT NULL
) STRICT;
