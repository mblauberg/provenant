-- Exact destination lookup and ordered pending reads, independent of history size.
CREATE INDEX human_local_delivery ON human_answer_outbox(project,
 json_extract(CAST(record AS TEXT),'$.target_host'),
 json_extract(CAST(record AS TEXT),'$.target_session'),
 json_extract(CAST(record AS TEXT),'$.target_actor'), answer
) WHERE json_extract(CAST(record AS TEXT),'$.state') IN ('queued','offered');
