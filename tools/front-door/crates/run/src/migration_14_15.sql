-- Base 15: local delivery receipts use JSON null instead of an empty remote.
-- A malformed row remains isolated, and already-current rows stay unchanged.
DROP TRIGGER IF EXISTS immutable_terminal_update;

UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.delivery.remote', NULL) AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.delivery.remote') = 'text'
  AND json_extract(CAST(record AS TEXT), '$.delivery.remote') = '';

UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.delivery.integration.remote', NULL) AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.delivery.integration.remote') = 'text'
  AND json_extract(CAST(record AS TEXT), '$.delivery.integration.remote') = '';

CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;