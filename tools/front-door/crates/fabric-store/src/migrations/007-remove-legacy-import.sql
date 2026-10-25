-- The one-shot legacy import is gone. Drop its marker table and the always-null
-- legacy fields it left in stored records. SQLite's JSON removal preserves the
-- remaining members verbatim, so records stay byte-identical to a fresh encode.
DROP TABLE imports;
UPDATE messages SET record = CAST(json_remove(CAST(record AS TEXT), '$.legacy', '$.legacy_output_path') AS BLOB);
UPDATE tasks SET record = CAST(json_remove(CAST(record AS TEXT), '$.legacy') AS BLOB);
