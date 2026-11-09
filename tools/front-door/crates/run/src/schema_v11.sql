-- Listing pages seek in sequence order without sorting retained history.
CREATE INDEX project_attempt_sequence ON attempts(project,sequence);
CREATE INDEX run_attempt_sequence ON attempts(project,run,sequence);
-- Refusal pages must not parse every successful operation's JSON at read time.
CREATE INDEX refused_operations ON operations(project,actor)
 WHERE json_extract(CAST(record AS TEXT),'$.state')='refused';
PRAGMA user_version=11;
