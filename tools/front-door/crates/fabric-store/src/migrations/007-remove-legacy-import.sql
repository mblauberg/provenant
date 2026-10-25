-- The one-shot legacy import is gone. Opening refuses this migration while any
-- data that import wrote remains (db.rs), so only always-null members are left.
-- Drop its marker table and strip those members wherever a Task or Message is
-- stored: records, coordination receipts (TaskResult) and committed peer replies
-- (PeerResponse carrying a TaskResult). json_remove keeps the other members
-- verbatim, so canonical records stay byte-identical to a fresh encode. Only
-- rows that hold a removed member are rewritten.
DROP TABLE imports;
UPDATE messages
 SET record = CAST(json_remove(CAST(record AS TEXT), '$.legacy', '$.legacy_output_path') AS BLOB)
 WHERE json_type(CAST(record AS TEXT), '$.legacy') IS NOT NULL
    OR json_type(CAST(record AS TEXT), '$.legacy_output_path') IS NOT NULL;
UPDATE tasks
 SET record = CAST(json_remove(CAST(record AS TEXT), '$.legacy') AS BLOB)
 WHERE json_type(CAST(record AS TEXT), '$.legacy') IS NOT NULL;
UPDATE coordination_operations
 SET result = CAST(json_remove(CAST(result AS TEXT), '$.task.legacy') AS BLOB)
 WHERE json_type(CAST(result AS TEXT), '$.task.legacy') IS NOT NULL;
UPDATE peer_operations
 SET response = CAST(json_remove(CAST(response AS TEXT), '$.result.data.result.task.legacy') AS BLOB)
 WHERE CASE WHEN json_valid(CAST(response AS TEXT))
  THEN json_type(CAST(response AS TEXT), '$.result.data.result.task.legacy') IS NOT NULL END;
