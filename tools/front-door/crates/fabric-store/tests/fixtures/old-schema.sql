-- Synthetic old-schema fixture: no production state or captured mail.
CREATE TABLE tasks(project TEXT, task_id TEXT, objective TEXT, owner_id TEXT, state TEXT, created_at INTEGER, updated_at INTEGER, PRIMARY KEY(project,task_id));
CREATE TABLE messages(message_id TEXT PRIMARY KEY, project TEXT, sender_id TEXT, body TEXT, kind TEXT, conversation_id TEXT, reply_to TEXT, task_id TEXT, output_path TEXT, created_at INTEGER);
CREATE TABLE deliveries(message_id TEXT, project TEXT, recipient_id TEXT, read_at INTEGER, PRIMARY KEY(message_id,recipient_id));
