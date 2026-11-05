-- Deliberately discard imported rows and references that current readers cannot
-- interpret. Null retired members on ordinary records are removed below.
DELETE FROM deliveries
 WHERE (project, message) IN (
   SELECT project, id FROM messages
   WHERE json_type(CAST(record AS TEXT), '$.legacy') NOT IN ('null')
      OR json_type(CAST(record AS TEXT), '$.legacy_output_path') NOT IN ('null')
      OR EXISTS (SELECT 1 FROM json_tree(CAST(record AS TEXT))
                 WHERE key = 'scope' AND value = 'legacy')
 );
DELETE FROM messages
 WHERE json_type(CAST(record AS TEXT), '$.legacy') NOT IN ('null')
    OR json_type(CAST(record AS TEXT), '$.legacy_output_path') NOT IN ('null')
    OR EXISTS (SELECT 1 FROM json_tree(CAST(record AS TEXT))
               WHERE key = 'scope' AND value = 'legacy');
DELETE FROM tasks
 WHERE json_type(CAST(record AS TEXT), '$.legacy') NOT IN ('null');
DELETE FROM events
 WHERE json_extract(CAST(record AS TEXT), '$.kind') = 'imported';
DELETE FROM coordination_operations
 WHERE EXISTS (SELECT 1 FROM json_tree(CAST(result AS TEXT))
               WHERE (key IN ('legacy', 'legacy_output_path') AND type <> 'null')
                  OR (key = 'scope' AND value = 'legacy'));
DELETE FROM peer_operations
 WHERE CASE WHEN json_valid(CAST(response AS TEXT)) THEN
   EXISTS (SELECT 1 FROM json_tree(CAST(response AS TEXT))
           WHERE (key IN ('legacy', 'legacy_output_path') AND type <> 'null')
              OR (key = 'scope' AND value = 'legacy')) END;
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
