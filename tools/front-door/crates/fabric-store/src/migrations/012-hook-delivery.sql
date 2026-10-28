-- Exclude terminal hook failures from indexed walks while preserving mail.
-- Keep the marker in sync with HOOK_ENVELOPE_TOO_LARGE in local_delivery.rs.
CREATE INDEX human_hook_queued ON human_answer_outbox(project,
 json_extract(CAST(record AS TEXT),'$.target_host'),
 json_extract(CAST(record AS TEXT),'$.target_session'),
 json_extract(CAST(record AS TEXT),'$.target_actor'), answer
) WHERE json_extract(CAST(record AS TEXT),'$.state')='queued'
 AND coalesce(json_extract(CAST(record AS TEXT),'$.error'),'') != 'hook_envelope_too_large';
CREATE INDEX human_hook_pending ON human_answer_outbox(project,
 json_extract(CAST(record AS TEXT),'$.target_host'),
 json_extract(CAST(record AS TEXT),'$.target_session'),
 json_extract(CAST(record AS TEXT),'$.target_actor'), answer
) WHERE json_extract(CAST(record AS TEXT),'$.state') IN ('queued','offered')
 AND coalesce(json_extract(CAST(record AS TEXT),'$.error'),'') != 'hook_envelope_too_large';
