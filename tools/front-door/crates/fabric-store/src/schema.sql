CREATE TABLE messages (
 project TEXT NOT NULL, id TEXT NOT NULL, record BLOB NOT NULL,
 PRIMARY KEY(project, id)
) STRICT;
CREATE TABLE delivery_sequences (
 project TEXT PRIMARY KEY, seq INTEGER NOT NULL CHECK(seq >= 0)
) STRICT;
CREATE TABLE deliveries (
 seq INTEGER NOT NULL CHECK(seq > 0),
 project TEXT NOT NULL, message TEXT NOT NULL, recipient TEXT NOT NULL,
 PRIMARY KEY(project, seq),
 UNIQUE(project, message, recipient),
 FOREIGN KEY(project, message) REFERENCES messages(project, id)
) STRICT;
CREATE INDEX inbox ON deliveries(project, recipient, seq);
CREATE TABLE mail_cursors (
 project TEXT NOT NULL, recipient TEXT NOT NULL, seq INTEGER NOT NULL CHECK(seq >= 0),
 PRIMARY KEY(project, recipient)
) STRICT;
CREATE TABLE events (
 seq INTEGER PRIMARY KEY AUTOINCREMENT, project TEXT NOT NULL,
 dedup TEXT, record BLOB NOT NULL, UNIQUE(project, dedup)
) STRICT;
CREATE INDEX event_cursor ON events(project, seq);
CREATE TABLE event_cursors (
 project TEXT NOT NULL, consumer TEXT NOT NULL, seq INTEGER NOT NULL CHECK(seq >= 0),
 PRIMARY KEY(project, consumer)
) STRICT;
CREATE TABLE claims (
 project TEXT NOT NULL, id TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind='work'),
 scope_key TEXT NOT NULL, generation INTEGER NOT NULL CHECK(generation >= 1),
 holder TEXT, token BLOB, expires INTEGER, record BLOB NOT NULL,
 PRIMARY KEY(project,id), UNIQUE(project,kind,scope_key),
 CHECK((holder IS NULL AND token IS NULL AND expires IS NULL)
    OR (holder IS NOT NULL AND length(token)=16 AND expires IS NOT NULL))
) STRICT;
CREATE TABLE claim_paths (
 project TEXT NOT NULL, claim TEXT NOT NULL, path TEXT NOT NULL,
 PRIMARY KEY(project,claim,path),
 FOREIGN KEY(project,claim) REFERENCES claims(project,id)
) STRICT;
CREATE TABLE claim_counters (
 project TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind='work'),
 scope_key TEXT NOT NULL, id TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation >= 1),
 PRIMARY KEY(project,kind,scope_key), UNIQUE(project,id)
) STRICT;
CREATE INDEX claim_path_lookup ON claim_paths(project,path,claim);
CREATE INDEX released_claims ON claims(project,id) WHERE holder IS NULL;
CREATE TABLE coordination_operations (
 project TEXT NOT NULL, id TEXT NOT NULL, actor TEXT NOT NULL,
 request BLOB NOT NULL, result BLOB NOT NULL,
 PRIMARY KEY(project,id)
) STRICT;
CREATE TABLE peer_operations (
 project TEXT NOT NULL, id TEXT NOT NULL, actor TEXT NOT NULL,
 digest TEXT NOT NULL,
 phase TEXT NOT NULL CHECK(phase IN ('prepared','sent','unknown','committed')),
 request BLOB NOT NULL, response BLOB,
 PRIMARY KEY(project,id),
 CHECK((phase='committed' AND response IS NOT NULL)
    OR (phase<>'committed' AND response IS NULL))
) STRICT;
CREATE TABLE run_event_cursors (
 project TEXT NOT NULL, host TEXT NOT NULL,
 sequence INTEGER NOT NULL CHECK(sequence >= 0),
 PRIMARY KEY(project,host)
) STRICT;
CREATE TABLE human_requests (
 project TEXT NOT NULL, id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision > 0),
 session TEXT NOT NULL, state TEXT NOT NULL, record BLOB NOT NULL,
 PRIMARY KEY(project,id)
) STRICT;
CREATE INDEX human_inbox ON human_requests(project,state,id);
CREATE INDEX human_session_inbox ON human_requests(project,session,state,id);
CREATE TABLE human_request_history (
 project TEXT NOT NULL, request TEXT NOT NULL, revision INTEGER NOT NULL, record BLOB NOT NULL,
 PRIMARY KEY(project,request,revision),
 FOREIGN KEY(project,request) REFERENCES human_requests(project,id)
) STRICT;
CREATE TABLE human_answers (
 project TEXT NOT NULL, id TEXT NOT NULL, request TEXT NOT NULL, record BLOB NOT NULL,
 PRIMARY KEY(project,id), UNIQUE(project,request),
 FOREIGN KEY(project,request) REFERENCES human_requests(project,id)
) STRICT;
CREATE TABLE human_answer_outbox (
 project TEXT NOT NULL, answer TEXT NOT NULL, record BLOB NOT NULL,
 PRIMARY KEY(project,answer),
 FOREIGN KEY(project,answer) REFERENCES human_answers(project,id)
) STRICT;
CREATE TABLE graph_chairs (
    project TEXT NOT NULL,
    graph TEXT NOT NULL,
    record BLOB NOT NULL,
    PRIMARY KEY(project, graph)
) STRICT;
CREATE INDEX human_local_delivery ON human_answer_outbox(project,
 json_extract(CAST(record AS TEXT),'$.target_host'),
 json_extract(CAST(record AS TEXT),'$.target_session'),
 json_extract(CAST(record AS TEXT),'$.target_actor'), answer
) WHERE json_extract(CAST(record AS TEXT),'$.state') IN ('queued','offered');
CREATE INDEX human_queued_delivery ON human_answer_outbox(project,
 json_extract(CAST(record AS TEXT),'$.target_host'),
 json_extract(CAST(record AS TEXT),'$.target_session'),
 json_extract(CAST(record AS TEXT),'$.target_actor'), answer
) WHERE json_extract(CAST(record AS TEXT),'$.state')='queued';
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
PRAGMA user_version=16;
