-- Remove retired import metadata and fields while preserving the remaining records.
DELETE FROM events
 WHERE json_extract(CAST(record AS TEXT), '$.kind') = 'imported'
   AND project IN (SELECT project FROM imports WHERE tasks = 0 AND messages = 0);
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
