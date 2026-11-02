-- Effect receipts are mutable independently of the immutable terminal record.
CREATE TABLE settlement_receipts (
 attempt TEXT PRIMARY KEY REFERENCES attempts(id),
 generation INTEGER NOT NULL,
 record BLOB NOT NULL CHECK(length(record)<=1048576)
) STRICT;
PRAGMA user_version=7;
