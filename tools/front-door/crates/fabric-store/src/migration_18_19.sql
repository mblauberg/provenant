-- Rename the embedded run-state and observation detail fields.
UPDATE events SET record = CAST(json_set(CAST(record AS TEXT), '$.detail', json_remove(json_insert(json_extract(CAST(record AS TEXT), '$.detail'), '$.event.change.cause', json_extract(json_extract(CAST(record AS TEXT), '$.detail'), '$.event.change.terminal_cause')), '$.event.change.terminal_cause') || '') AS BLOB)
WHERE json_valid(CAST(record AS TEXT)) AND json_valid(json_extract(CAST(record AS TEXT), '$.detail'))
  AND json_type(json_extract(CAST(record AS TEXT), '$.detail'), '$.event.change.terminal_cause') IS NOT NULL;
UPDATE events SET record = CAST(json_set(CAST(record AS TEXT), '$.detail', json_set(json_extract(CAST(record AS TEXT), '$.detail'), '$.event.change.cause', 'launch_handoff_refused') || '') AS BLOB)
WHERE json_valid(CAST(record AS TEXT)) AND json_valid(json_extract(CAST(record AS TEXT), '$.detail'))
  AND json_extract(json_extract(CAST(record AS TEXT), '$.detail'), '$.event.change.cause') = 'handoff_refused';
UPDATE events SET record = CAST(json_set(CAST(record AS TEXT), '$.detail', json_remove(json_insert(json_extract(CAST(record AS TEXT), '$.detail'), '$.detail.cause', json_extract(json_extract(CAST(record AS TEXT), '$.detail'), '$.detail.terminal_cause')), '$.detail.terminal_cause') || '') AS BLOB)
WHERE json_valid(CAST(record AS TEXT)) AND json_valid(json_extract(CAST(record AS TEXT), '$.detail'))
  AND json_type(json_extract(CAST(record AS TEXT), '$.detail'), '$.detail.terminal_cause') IS NOT NULL;
UPDATE events SET record = CAST(json_set(CAST(record AS TEXT), '$.detail', json_set(json_extract(CAST(record AS TEXT), '$.detail'), '$.detail.cause', 'launch_handoff_refused') || '') AS BLOB)
WHERE json_valid(CAST(record AS TEXT)) AND json_valid(json_extract(CAST(record AS TEXT), '$.detail'))
  AND json_extract(json_extract(CAST(record AS TEXT), '$.detail'), '$.detail.cause') = 'handoff_refused';
