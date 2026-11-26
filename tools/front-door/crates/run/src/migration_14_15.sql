-- Base 15: rename settlement fields while retaining terminal row immutability.
DROP TRIGGER IF EXISTS immutable_terminal_update;

UPDATE attempts
SET record = CAST(json_remove(json_insert(CAST(record AS TEXT), '$.state.cause', json_extract(CAST(record AS TEXT), '$.state.terminal_cause')), '$.state.terminal_cause') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.state.terminal_cause') IS NOT NULL;

UPDATE attempts
SET record = CAST(json_remove(json_insert(CAST(record AS TEXT), '$.signals', json_extract(CAST(record AS TEXT), '$.partial_signals')), '$.partial_signals') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.partial_signals') IS NOT NULL;

UPDATE attempts
SET record = CAST(json_remove(json_insert(CAST(record AS TEXT), '$.terminal_basis', json_extract(CAST(record AS TEXT), '$.terminal_evidence')), '$.terminal_evidence') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.terminal_evidence') IS NOT NULL;

UPDATE attempts
SET record = CAST(json_remove(json_insert(CAST(record AS TEXT), '$.settlement_class', json_extract(CAST(record AS TEXT), '$.settlement_reason')), '$.settlement_reason') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.settlement_reason') IS NOT NULL;

UPDATE attempts SET record = CAST(json_set(CAST(record AS TEXT), '$.state.cause', 'launch_handoff_refused') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.state.cause') = 'handoff_refused';

-- Run events also embed AttemptState under change.
UPDATE events SET record = CAST(json_remove(json_insert(CAST(record AS TEXT), '$.change.cause', json_extract(CAST(record AS TEXT), '$.change.terminal_cause')), '$.change.terminal_cause') AS BLOB)
WHERE json_valid(CAST(record AS TEXT)) AND json_type(CAST(record AS TEXT), '$.change.terminal_cause') IS NOT NULL;
UPDATE events SET record = CAST(json_set(CAST(record AS TEXT), '$.change.cause', 'launch_handoff_refused') AS BLOB)
WHERE json_valid(CAST(record AS TEXT)) AND json_extract(CAST(record AS TEXT), '$.change.cause') = 'handoff_refused';

CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
