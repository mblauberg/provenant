-- Base 13: attempts carry no retired lane-cache grant.
--
-- Base 12 rows recorded the lane Cargo cache as a write grant after #3802
-- removed it. Rewrite only what the current types cannot read: filter
-- lane_cache grants without touching another grant (a grant with no source
-- keeps its own decode failure). The retired profile.launch_policy snapshot is
-- left in place: the current type decodes it as an extra field and the sealed
-- request keeps the authoritative copy. The grant walk is total for any valid
-- JSON: only an array is walked and only object elements are inspected. Rows
-- that are already valid JSON but need no change are left byte-identical. A
-- malformed row is skipped so it fails only its own decode instead of aborting
-- every writable open. Terminal rows are immutable to ordinary writers, so the
-- trigger is lifted only inside the migration transaction.
DROP TRIGGER IF EXISTS immutable_terminal_update;
UPDATE attempts
SET record = CAST(
    json_set(
        CAST(record AS TEXT),
        '$.write_grants',
        json(COALESCE((
            SELECT json_group_array(json(json_each.value))
            FROM json_each(CAST(record AS TEXT), '$.write_grants')
            WHERE CASE WHEN json_each.type = 'object'
                       THEN json_extract(json_each.value, '$.source') IS NOT 'lane_cache'
                       ELSE 0
                       END
        ), '[]'))
    ) AS BLOB)
WHERE record IS NOT NULL
  AND CASE
      WHEN json_valid(CAST(record AS TEXT)) THEN
          json_type(CAST(record AS TEXT), '$.write_grants') IS 'array'
          AND EXISTS (
              SELECT 1
              FROM json_each(CAST(record AS TEXT), '$.write_grants')
              WHERE CASE WHEN json_each.type = 'object'
                         THEN json_extract(json_each.value, '$.source') IS 'lane_cache'
                         ELSE 0
                         END
          )
      ELSE 0
      END;
CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
