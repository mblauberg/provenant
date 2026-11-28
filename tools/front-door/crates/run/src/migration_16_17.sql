-- Version 16 stores did not rewrite these resource origins in 13 -> 14.
DROP TRIGGER IF EXISTS immutable_terminal_update;
UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.skipped_routes', json((
    SELECT json_group_array(CASE
        WHEN json_extract(value, '$.guard_resource.kind') IN ('pacing_reserve', 'lane_ports')
         AND json_extract(value, '$.guard_resource.origin') = 'configuration'
            THEN json(json_set(value, '$.guard_resource.origin', 'host'))
        WHEN json_extract(value, '$.guard_resource.kind') IN ('pacing_reserve', 'lane_ports')
         AND json_extract(value, '$.guard_resource.origin') = 'shipped_default'
            THEN json(json_set(value, '$.guard_resource.origin', 'default'))
        ELSE json(value) END)
    FROM (SELECT value FROM json_each(CAST(record AS TEXT), '$.skipped_routes') ORDER BY key)
))) AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.skipped_routes') = 'array'
  AND EXISTS (SELECT 1 FROM json_each(CAST(record AS TEXT), '$.skipped_routes')
    WHERE json_extract(value, '$.guard_resource.kind') IN ('pacing_reserve', 'lane_ports')
      AND json_extract(value, '$.guard_resource.origin') IN ('configuration', 'shipped_default'));
CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
