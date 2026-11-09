-- The shared task board is removed. GitHub issues and graph nodes track work.
-- Drop the board, the events and replay receipts that only it could read, and
-- the optional task link on messages so mail stays readable and replayable.
DROP TABLE tasks;
DELETE FROM events
 WHERE CASE WHEN json_valid(CAST(record AS TEXT))
  THEN json_extract(CAST(record AS TEXT), '$.kind') IN
   ('task_created','task_claimed','task_updated','task_renewed','task_released','task_finished') END;
DELETE FROM coordination_operations
 WHERE CASE WHEN json_valid(CAST(request AS TEXT))
  THEN json_extract(CAST(request AS TEXT), '$.domain') = 'task' END;
DELETE FROM peer_operations
 WHERE CASE WHEN json_valid(CAST(request AS TEXT)) THEN
   EXISTS (SELECT 1 FROM json_tree(CAST(request AS TEXT))
           WHERE key = 'domain' AND value = 'task') END;
UPDATE messages
 SET record = CAST(json_remove(CAST(record AS TEXT), '$.task') AS BLOB)
 WHERE CASE WHEN json_valid(CAST(record AS TEXT))
  THEN json_type(CAST(record AS TEXT), '$.task') IS NOT NULL END;
UPDATE coordination_operations
 SET request = CAST(json_remove(CAST(request AS TEXT), '$.input.task') AS BLOB)
 WHERE CASE WHEN json_valid(CAST(request AS TEXT))
  THEN json_extract(CAST(request AS TEXT), '$.domain') = 'mail'
   AND json_type(CAST(request AS TEXT), '$.input.task') IS NOT NULL END;
