-- Base 13: attempts carry no launch policy and no retired lane-cache grant.
--
-- Base 12 rows copied the resolved policy into profile.launch_policy, whose
-- boundary vocabulary changed (git_isolation and data_integrity became
-- other_work), and recorded the lane Cargo cache as a write grant after #3802
-- removed it. Rewrite every affected row so it decodes under the current
-- attempt types. Terminal rows are immutable to ordinary writers, so the
-- trigger is lifted only inside the migration transaction.
DROP TRIGGER IF EXISTS immutable_terminal_update;
UPDATE attempts
SET record = CAST(json_remove(
    json_set(
        CAST(record AS TEXT),
        '$.write_grants',
        json((
            SELECT json_group_array(json(value))
            FROM json_each(CAST(record AS TEXT), '$.write_grants')
            WHERE json_extract(value, '$.source') <> 'lane_cache'
        ))
    ),
    '$.profile.launch_policy'
) AS BLOB)
WHERE record IS NOT NULL
  AND (
    json_type(CAST(record AS TEXT), '$.profile.launch_policy') IS NOT NULL
    OR EXISTS (
        SELECT 1
        FROM json_each(CAST(record AS TEXT), '$.write_grants')
        WHERE json_extract(value, '$.source') = 'lane_cache'
    )
  );
CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
