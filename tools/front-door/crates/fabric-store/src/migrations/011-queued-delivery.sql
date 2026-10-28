-- Once-only hook candidates must not scan previously offered answers.
CREATE INDEX human_queued_delivery ON human_answer_outbox(project,
 json_extract(CAST(record AS TEXT),'$.target_host'),
 json_extract(CAST(record AS TEXT),'$.target_session'),
 json_extract(CAST(record AS TEXT),'$.target_actor'), answer
) WHERE json_extract(CAST(record AS TEXT),'$.state')='queued';
