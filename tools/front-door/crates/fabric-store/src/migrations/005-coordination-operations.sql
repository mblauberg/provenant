-- Receipts commit in the same transaction as coordination effects/events.
CREATE TABLE coordination_operations (
 project TEXT NOT NULL, id TEXT NOT NULL, actor TEXT NOT NULL,
 request BLOB NOT NULL, result BLOB NOT NULL,
 PRIMARY KEY(project,id)
) STRICT;
