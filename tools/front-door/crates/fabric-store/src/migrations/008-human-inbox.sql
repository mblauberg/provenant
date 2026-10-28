-- Human choices are records, never run/workflow authority.
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
