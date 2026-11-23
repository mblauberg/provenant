-- Base 13: attempts carry no launch policy and no retired lane-cache grant.
--
-- Base 12 rows copied the resolved policy into profile.launch_policy, whose
-- boundary vocabulary changed (git_isolation and data_integrity became
-- other_work), and recorded the lane Cargo cache as a write grant after #3802
-- removed it. Rewrite only what the current types cannot read: drop the policy
-- snapshot, and filter lane_cache grants without touching another grant (a
-- grant with no source keeps its own decode failure). Rows that are already
-- valid JSON but need no change are left byte-identical. A malformed row is
-- skipped so it fails only its own decode instead of aborting every writable
-- open. Terminal rows are immutable to ordinary writers, so the trigger is
-- lifted only inside the migration transaction.
DROP TRIGGER IF EXISTS immutable_terminal_update;
UPDATE attempts
SET record = CAST(
    CASE
        WHEN EXISTS (
            SELECT 1
            FROM json_each(CAST(record AS TEXT), '$.write_grants')
            WHERE json_extract(value, '$.source') IS 'lane_cache'
        )
        THEN json_remove(
            json_set(
                CAST(record AS TEXT),
                '$.write_grants',
                json((
                    SELECT json_group_array(json(value))
                    FROM json_each(CAST(record AS TEXT), '$.write_grants')
                    WHERE json_extract(value, '$.source') IS NOT 'lane_cache'
                ))
            ),
            '$.profile.launch_policy'
        )
        ELSE json_remove(CAST(record AS TEXT), '$.profile.launch_policy')
    END AS BLOB)
WHERE record IS NOT NULL
  AND CASE
      WHEN json_valid(CAST(record AS TEXT)) THEN
          json_type(CAST(record AS TEXT), '$.profile.launch_policy') IS NOT NULL
          OR EXISTS (
              SELECT 1
              FROM json_each(CAST(record AS TEXT), '$.write_grants')
              WHERE json_extract(value, '$.source') IS 'lane_cache'
          )
      ELSE 0
      END;
CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
